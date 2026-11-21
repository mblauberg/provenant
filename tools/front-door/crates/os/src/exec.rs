//! Bounded direct-child utility execution. Provider custody belongs to S2.
use crate::capture::{FrameDelivery, StreamCapture};
#[cfg(test)]
pub(crate) use crate::capture::{Frames, Pump};
use crate::identity::Signal;
use crate::sys::{raw_exec::OwnedPid as RawChild, reaper::Reaper, resources::ProcessBudget};
use provenant_core::CancelToken;
pub use provenant_core::capture::{CaptureFilter, FRAME_LIMIT, FilterError};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(test, feature = "fixture"))]
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};
use thiserror::Error;

/// Scoped signal observation while a build waits for admission.
pub struct BuildSignalWait(crate::sys::build_group::Forwarder);
impl BuildSignalWait {
    pub fn install() -> std::io::Result<Self> {
        crate::sys::build_group::Forwarder::install().map(Self)
    }
    pub fn signal(&self) -> Option<i32> {
        match self.0.signal() {
            0 => None,
            signal => Some(signal),
        }
    }
}

pub const DEFAULT_CAPTURE_LIMIT: usize = 8 * 1024 * 1024;
pub const MAX_CAPTURE_LIMIT: usize = 64 * 1024 * 1024;
/// Maximum source bytes in a structured provider protocol line.
pub const PROVIDER_LINE_LIMIT: usize = 1_048_576;
const DETACHED_RECEIPT_LIMIT: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Termination {
    Exit(i64),
    Signal(i64),
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedStream {
    pub bytes: Vec<u8>,
    pub total_bytes: u64,
    pub truncated: bool,
    pub sink_lost_bytes: u64,
    pub sink_failed: bool,
    pub oversized_frames: u64,
    pub decoder_lost_frames: u64,
    pub read_failed: bool,
    pub filter_failed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecOutcome {
    pub termination: Termination,
    pub stdout: CapturedStream,
    pub stderr: CapturedStream,
    /// True only when wall expiry caused TERM delivery to the owned child.
    pub timed_out: bool,
    pub drain_truncated: bool,
    pub overflow_stopped: bool,
    pub cancelled: bool,
    pub elapsed_micros: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueLoss {
    Full,
    Closed,
}
/// A real transport seam: implementations must return immediately and retain
/// at most their declared queue bound. Never call arbitrary blocking Write here.
pub trait NonblockingSink: Send + Sync {
    fn try_send(&self, bytes: &[u8]) -> Result<(), QueueLoss>;
}
#[cfg(any(test, feature = "fixture"))]
pub struct TeeQueue(SyncSender<Vec<u8>>);
#[cfg(any(test, feature = "fixture"))]
impl NonblockingSink for TeeQueue {
    fn try_send(&self, bytes: &[u8]) -> Result<(), QueueLoss> {
        if bytes.len() > FRAME_LIMIT {
            return Err(QueueLoss::Full);
        }
        self.0
            .try_send(bytes.to_vec())
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => QueueLoss::Full,
                mpsc::TrySendError::Disconnected(_) => QueueLoss::Closed,
            })
    }
}
#[cfg(any(test, feature = "fixture"))]
pub fn tee_queue(frames: usize) -> Result<(TeeQueue, Receiver<Vec<u8>>), ExecError> {
    if frames == 0 || frames > 1024 {
        return Err(ExecError::InvalidLimit);
    }
    let (sender, receiver) = mpsc::sync_channel(frames);
    Ok((TeeQueue(sender), receiver))
}
pub enum StreamPolicy<'a> {
    Capture {
        limit: usize,
    },
    /// Inherit the caller's output descriptor without capturing it.
    Inherit,
    /// Retain the most recent bytes while continuing to drain the stream.
    Tail {
        limit: usize,
    },
    Discard,
    Tee {
        limit: usize,
        sink: &'a dyn NonblockingSink,
    },
}
impl Default for StreamPolicy<'_> {
    fn default() -> Self {
        Self::Capture {
            limit: DEFAULT_CAPTURE_LIMIT,
        }
    }
}
impl StreamPolicy<'_> {
    pub(crate) fn limit(&self) -> usize {
        match self {
            Self::Capture { limit } | Self::Tail { limit } | Self::Tee { limit, .. } => *limit,
            Self::Inherit => 0,
            Self::Discard => 0,
        }
    }
    pub(crate) fn piped(&self) -> bool {
        !matches!(self, Self::Discard | Self::Inherit)
    }

    fn inherited(&self) -> bool {
        matches!(self, Self::Inherit)
    }
}

/// Why a call site keeps raw captured bytes instead of the executor's default
/// local-disclosure mask. The reason is part of the type so an opt-out is
/// auditable; a bare boolean would let a site disable masking silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawCaptureReason {
    /// The captured bytes are parsed byte-for-byte (paths, object ids,
    /// protocol fields), so a mask could change their meaning.
    ByteExactParse,
    /// A higher layer owns the redaction boundary for these bytes.
    CallerOwnsRedaction,
}

/// Per-stream masking policy for captured bytes. Masking runs on the stream,
/// before the capture bound, so a secret that straddles the bound cannot leave
/// a fragment in the retained bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureMask {
    /// Mask captured bytes with local-disclosure redaction. This applies to
    /// every capture by default.
    #[default]
    Mask,
    /// Keep raw bytes for the named reason.
    Raw(RawCaptureReason),
}

/// Frames are decoded before retention and redaction; full queues are loss,
/// never a synthetic terminal or a process stop request.
pub trait FrameSink: Send + Sync {
    fn try_frame(&self, bytes: &[u8]) -> Result<(), QueueLoss>;
}
/// Memory-only bytes for one inherited read pipe. Only its descriptor number
/// enters the child's environment; the write end closes before spawn.
#[derive(Clone, Serialize, Deserialize)]
pub struct SecretInput {
    pub descriptor_env: String,
    pub bytes: Vec<u8>,
}
impl std::fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretInput(redacted)")
    }
}

/// Memory-only values retained by the guardian's publication filter, never
/// passed to the provider or included in a launch summary.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SecretValues(pub Vec<Vec<u8>>);
impl std::fmt::Debug for SecretValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValues(redacted)")
    }
}

pub struct ExecOptions<'a> {
    pub argv: &'a [OsString],
    pub env: Option<&'a [(OsString, OsString)]>,
    pub cwd: &'a Path,
    pub stdin: Option<&'a [u8]>,
    pub secret_input: Option<&'a SecretInput>,
    /// Pass a lane or updater lease through exec, including to descendants.
    pub inherited_lease: Option<&'a crate::fs::LockGuard>,
    /// Verified profile custody; prepare a read pipe at the final exec edge.
    pub inherited_profile: Option<&'a dyn crate::sys::profile::LimitSource>,
    /// Run in a private session and kill the whole group immediately on timeout.
    pub process_group: bool,
    /// Run in a private session and kill remaining group members on every exit,
    /// including success. Cleanup occurs before the leader's PID is reaped.
    pub cleanup_process_group: bool,
    /// A process-group watchdog survives this executor's death and enforces
    /// the timeout. Requires `process_group` and group cleanup on every exit.
    pub group_watchdog: bool,
    pub stdout: StreamPolicy<'a>,
    pub stderr: StreamPolicy<'a>,
    /// Masking policy for retained stdout bytes. Defaults to `CaptureMask::Mask`.
    pub stdout_mask: CaptureMask,
    /// Masking policy for retained stderr bytes. Defaults to `CaptureMask::Mask`.
    pub stderr_mask: CaptureMask,
    pub stdout_decoder: Option<&'a dyn FrameSink>,
    pub stderr_decoder: Option<&'a dyn FrameSink>,
    pub stdout_filter: Option<&'a mut dyn CaptureFilter>,
    pub stderr_filter: Option<&'a mut dyn CaptureFilter>,
    /// Observe the child PID immediately after spawn, before waiting or reading output.
    pub on_spawn: Option<&'a mut dyn FnMut(u32)>,
    /// Synchronously write filtered stream bytes without a lossy tee queue.
    pub stdout_writer: Option<&'a mut dyn std::io::Write>,
    pub stderr_writer: Option<&'a mut dyn std::io::Write>,
    pub cancel: Option<&'a CancelToken>,
    pub timeout: Duration,
    /// Once spawned, finish the operation even after its soft deadline or cancellation.
    /// For non-interruptible transactions such as single-use credential rotation.
    pub wait_for_completion: bool,
    pub terminate_grace: Duration,
    pub reap_timeout: Duration,
    pub stop_on_output_limit: bool,
}
impl<'a> ExecOptions<'a> {
    pub fn new(argv: &'a [OsString], cwd: &'a Path, timeout: Duration) -> Self {
        Self {
            argv,
            env: None,
            cwd,
            stdin: None,
            secret_input: None,
            inherited_lease: None,
            inherited_profile: None,
            process_group: false,
            cleanup_process_group: false,
            group_watchdog: false,
            stdout: StreamPolicy::default(),
            stderr: StreamPolicy::default(),
            stdout_mask: CaptureMask::default(),
            stderr_mask: CaptureMask::default(),
            stdout_decoder: None,
            stderr_decoder: None,
            stdout_filter: None,
            stderr_filter: None,
            on_spawn: None,
            stdout_writer: None,
            stderr_writer: None,
            cancel: None,
            timeout,
            wait_for_completion: false,
            terminate_grace: Duration::from_millis(250),
            reap_timeout: Duration::from_millis(250),
            stop_on_output_limit: false,
        }
    }
}
#[derive(Debug, Error)]
pub enum ExecError {
    #[error("empty command argv")]
    EmptyArgv,
    #[error("execution duration outside the supported clock range")]
    DurationOutOfRange,
    #[error("capture or queue limit exceeds its bound")]
    InvalidLimit,
    #[error(
        "a group watchdog requires process_group, cleanup_process_group and an interruptible deadline"
    )]
    WatchdogRequiresGroupCleanup,
    #[error("capture stream has already been finished")]
    CaptureFinished,
    #[error("utility cleanup admission is full")]
    Busy,
    #[error("utility cancelled before spawn")]
    Cancelled,
    #[error("host child-wait policy cannot preserve utility custody")]
    ChildWaitPolicy(#[source] io::Error),
    #[error("utility cleanup transferred to the reserved reaper")]
    CleanupPending,
    #[error("refusing to start a detached worker from a test harness")]
    TestHarness,
    #[error(
        "user process count {used} of {limit}; wait for processes to exit before starting a worker"
    )]
    ProcessBudget { used: u64, limit: u64 },
    #[error("utility {operation} failed")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("prepared Git reference transaction is not supported by this executor")]
    PreparedRefTransactionUnsupported,
    #[error("prepared Git reference transaction options are invalid")]
    InvalidPreparedRefTransaction,
}
fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> ExecError {
    move |source| ExecError::Io { operation, source }
}
pub trait Executor: Send + Sync {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError>;

    /// Execute the initial `git update-ref --stdin` transaction commands up
    /// through `prepare`, then ask whether to commit while Git still holds the
    /// reference locks. `commit_after_prepare` returns true to commit and false
    /// to abort. The executor owns child timeout, cancellation, stream draining
    /// and cleanup. The synchronous decision callback is not preempted, so
    /// callers must keep it bounded. Deadline and cancellation are checked
    /// before and after it; a late or cancelled decision cannot commit.
    fn execute_prepared_ref_transaction(
        &self,
        options: ExecOptions<'_>,
        commands_through_prepare: &[u8],
        commit_after_prepare: &mut dyn FnMut() -> bool,
    ) -> Result<ExecOutcome, ExecError> {
        let _ = (options, commands_through_prepare, commit_after_prepare);
        Err(ExecError::PreparedRefTransactionUnsupported)
    }
}
/// Construct once at the process edge; all clones share one admission queue.
/// The process-wide reaper worker is shared by every instance. The host must
/// never set SIGCHLD to SIG_IGN or enable SA_NOCLDWAIT, and must never
/// independently wait for owned children. An installed SIGCHLD handler must
/// not call waitpid(-1), which could steal a child before the reaper observes it.
/// Each spawn checks the incompatible SIGCHLD policies before creating a child.
#[derive(Clone, Debug)]
pub struct SystemExecutor {
    reaper: Reaper,
}
impl SystemExecutor {
    pub fn new(capacity: usize) -> Result<Self, ExecError> {
        Ok(Self {
            reaper: Reaper::new(capacity).map_err(io_error("start reaper"))?,
        })
    }

    /// Run a private process group, forwarding caller INT, TERM and HUP to
    /// every active forwarding group and applying the configured grace period.
    pub fn execute_forwarding_group(
        &self,
        options: ExecOptions<'_>,
    ) -> Result<ExecOutcome, ExecError> {
        run_inner(&self.reaper, options, None, true)
    }
}

/// Start a one-shot worker in its own session with no terminal or pipe handles.
/// The process reaper owns its eventual exit while the caller remains alive.
pub fn spawn_detached(argv: &[OsString], cwd: &Path) -> Result<(), ExecError> {
    // Workers relaunch `current_exe`. A Cargo test harness reads worker
    // arguments as test filters, so each relaunch reruns the tests that spawned
    // it and the processes multiply until the host process table is full.
    if argv
        .first()
        .is_some_and(|program| is_test_harness(Path::new(program)))
    {
        return Err(ExecError::TestHarness);
    }
    check_process_budget(crate::sys::resources::process_budget())?;
    crate::sys::raw_exec::verify_child_wait_policy().map_err(ExecError::ChildWaitPolicy)?;
    let reaper = Reaper::new(1).map_err(io_error("start reaper"))?;
    let permit = reaper.reserve().ok_or(ExecError::Busy)?;
    let child = RawChild::spawn(
        permit,
        crate::sys::raw_exec::SpawnSpec {
            argv,
            env: None,
            cwd,
            input: false,
            secret_input: None,
            inherited_lease: None,
            inherited_profile: None,
            inherited_ports: None,
            process_group: true,
            capture_stdout: false,
            capture_stderr: false,
        },
    )
    .map_err(io_error("spawn detached worker"))?;
    drop(child); // Transfer custody without waiting or signalling the worker.
    Ok(())
}

/// Start a worker in its own process group and read one bounded receipt. When
/// the reader's deadline expires, the reaper retains the child without
/// signalling it: its work and any custody it holds must continue. `None` means
/// only that the deadline expired. A worker that ends or breaks its stream
/// early gives whatever it wrote, which the caller must validate.
#[allow(clippy::disallowed_methods)] // This is a real process deadline.
pub fn detached_receipt(
    argv: &[OsString],
    cwd: &Path,
    timeout: Duration,
) -> Result<Option<Vec<u8>>, ExecError> {
    if argv
        .first()
        .is_some_and(|program| is_test_harness(Path::new(program)))
    {
        return Err(ExecError::TestHarness);
    }
    check_process_budget(crate::sys::resources::process_budget())?;
    crate::sys::raw_exec::verify_child_wait_policy().map_err(ExecError::ChildWaitPolicy)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(ExecError::DurationOutOfRange)?;
    let reaper = Reaper::new(1).map_err(io_error("start reaper"))?;
    let permit = reaper.reserve().ok_or(ExecError::Busy)?;
    let mut child = RawChild::spawn(
        permit,
        crate::sys::raw_exec::SpawnSpec {
            argv,
            env: None,
            cwd,
            input: false,
            secret_input: None,
            inherited_lease: None,
            inherited_profile: None,
            inherited_ports: None,
            process_group: true,
            capture_stdout: true,
            capture_stderr: false,
        },
    )
    .map_err(io_error("spawn detached worker"))?;
    let mut output = child.stdout.take().ok_or(ExecError::EmptyArgv)?;
    Ok(receipt_until(deadline, Instant::now, |buffer, expired| {
        match child.ready(Some(&output), None, None) {
            Ok(true) => {}
            Ok(false) => return ReceiptStep::Pending,
            Err(_) => return ReceiptStep::End,
        }
        if expired() {
            return ReceiptStep::Expired;
        }
        match output.read(buffer) {
            Ok(0) => ReceiptStep::End,
            Ok(count) => ReceiptStep::Bytes(count),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                ReceiptStep::Retry
            }
            Err(_) => ReceiptStep::End,
        }
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceiptStep {
    Pending,
    Bytes(usize),
    End,
    Expired,
    Retry,
}

fn receipt_until<T: Copy + Ord>(
    deadline: T,
    mut now: impl FnMut() -> T,
    mut next: impl FnMut(&mut [u8], &mut dyn FnMut() -> bool) -> ReceiptStep,
) -> Option<Vec<u8>> {
    let mut receipt = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if now() >= deadline {
            return None;
        }
        let mut expired = || now() >= deadline;
        let step = next(&mut buffer, &mut expired);
        match step {
            ReceiptStep::Pending | ReceiptStep::Retry => continue,
            ReceiptStep::End => return Some(receipt),
            ReceiptStep::Expired => return None,
            ReceiptStep::Bytes(count) => {
                receipt.extend_from_slice(&buffer[..count]);
                if receipt.len() > DETACHED_RECEIPT_LIMIT || receipt.last() == Some(&b'\n') {
                    return Some(receipt);
                }
            }
        }
    }
}

/// Refuse a detached worker when the user's process table has no headroom. A
/// failed probe does not block: the limit is unknown, and the spawn itself
/// still fails if the table is full.
#[allow(clippy::print_stderr)]
fn check_process_budget(probe: io::Result<ProcessBudget>) -> Result<(), ExecError> {
    match probe {
        Ok(budget) if !budget.has_room() => Err(ExecError::ProcessBudget {
            used: budget.used,
            limit: budget.limit,
        }),
        Err(error) => {
            eprintln!("detached worker: process budget probe failed: {error}");
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Cargo builds test executables, and only those, into an intermediate
/// directory rather than the profile root it copies binaries to: a profile's
/// `deps`, or `build/<package>/<hash>/out` in the build-directory layout
/// Cargo 1.100 adopted.
fn is_test_harness(program: &Path) -> bool {
    let mut directories = program.ancestors().skip(1).map(Path::file_name);
    match directories.next().flatten() {
        Some(parent) if parent == "deps" => true,
        Some(parent) if parent == "out" => directories
            .nth(2)
            .flatten()
            .is_some_and(|build| build == "build"),
        _ => false,
    }
}

impl Executor for SystemExecutor {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
        run(&self.reaper, options)
    }

    fn execute_prepared_ref_transaction(
        &self,
        options: ExecOptions<'_>,
        commands_through_prepare: &[u8],
        commit_after_prepare: &mut dyn FnMut() -> bool,
    ) -> Result<ExecOutcome, ExecError> {
        run_prepared_ref_transaction(
            &self.reaper,
            options,
            commands_through_prepare,
            commit_after_prepare,
        )
    }
}
fn run(reaper: &Reaper, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
    run_inner(reaper, options, None, false)
}

struct PreparedRefTransaction<'a> {
    commands_through_prepare: &'a [u8],
    commit_after_prepare: &'a mut dyn FnMut() -> bool,
}

struct PreparedRefAck(AtomicBool);

impl PreparedRefAck {
    fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn observed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

fn prepared_ref_commit_allowed(requested: bool, cancelled: bool, before_deadline: bool) -> bool {
    requested && !cancelled && before_deadline
}

impl FrameSink for PreparedRefAck {
    fn try_frame(&self, bytes: &[u8]) -> Result<(), QueueLoss> {
        if bytes == b"prepare: ok" {
            self.0.store(true, Ordering::Release);
        }
        Ok(())
    }
}

fn run_prepared_ref_transaction(
    reaper: &Reaper,
    options: ExecOptions<'_>,
    commands_through_prepare: &[u8],
    commit_after_prepare: &mut dyn FnMut() -> bool,
) -> Result<ExecOutcome, ExecError> {
    if options.stdin.is_some()
        || !options.stdout.piped()
        || options.stdout_decoder.is_some()
        || options.stdout_filter.is_some()
        || options.secret_input.is_some()
        || options.wait_for_completion
        || commands_through_prepare.len() > 4096
        || !commands_through_prepare.ends_with(b"prepare\n")
    {
        return Err(ExecError::InvalidPreparedRefTransaction);
    }
    run_inner(
        reaper,
        options,
        Some(PreparedRefTransaction {
            commands_through_prepare,
            commit_after_prepare,
        }),
        false,
    )
}

// Process supervision deadlines use the operating system's monotonic clock.
#[allow(clippy::disallowed_methods)]
fn run_inner<'a>(
    reaper: &Reaper,
    mut options: ExecOptions<'a>,
    mut transaction: Option<PreparedRefTransaction<'_>>,
    forward_signals: bool,
) -> Result<ExecOutcome, ExecError> {
    if options.argv.is_empty() {
        return Err(ExecError::EmptyArgv);
    }
    if options.stdout.limit() > MAX_CAPTURE_LIMIT || options.stderr.limit() > MAX_CAPTURE_LIMIT {
        return Err(ExecError::InvalidLimit);
    }
    if options.group_watchdog
        && (options.wait_for_completion
            || !(options.process_group && options.cleanup_process_group))
    {
        return Err(ExecError::WatchdogRequiresGroupCleanup);
    }
    if forward_signals && !options.process_group {
        return Err(ExecError::WatchdogRequiresGroupCleanup);
    }
    if options.cancel.is_some_and(CancelToken::is_cancelled) {
        return Err(ExecError::Cancelled);
    }
    crate::sys::raw_exec::verify_child_wait_policy().map_err(ExecError::ChildWaitPolicy)?;
    let started = Instant::now();
    let deadline = started
        .checked_add(options.timeout)
        .ok_or(ExecError::DurationOutOfRange)?;
    deadline
        .checked_add(options.terminate_grace)
        .and_then(|at| at.checked_add(options.reap_timeout))
        .ok_or(ExecError::DurationOutOfRange)?;
    let permit = reaper.reserve().ok_or(ExecError::Busy)?;
    let signal_forwarder = if forward_signals {
        Some(
            crate::sys::build_group::Forwarder::install()
                .map_err(io_error("install signal forwarding"))?,
        )
    } else {
        None
    };
    let mut child = RawChild::spawn_with_watchdog(
        permit,
        crate::sys::raw_exec::SpawnSpec {
            argv: options.argv,
            env: options.env,
            cwd: options.cwd,
            input: options.stdin.is_some() || transaction.is_some(),
            secret_input: options.secret_input,
            inherited_lease: options.inherited_lease,
            inherited_profile: options.inherited_profile,
            inherited_ports: None,
            process_group: options.process_group || options.cleanup_process_group,
            capture_stdout: options.stdout.piped(),
            capture_stderr: options.stderr.piped(),
        },
        options.group_watchdog.then_some(options.timeout),
        options.stdout.inherited(),
        options.stderr.inherited(),
    )
    .map_err(io_error("spawn"))?;
    if let (Some(callback), Some(pid)) = (options.on_spawn.as_deref_mut(), child.id()) {
        callback(pid);
    }
    child.cleanup_process_group_on_exit(options.cleanup_process_group);
    if let Some(forwarder) = &signal_forwarder
        && let Some(pid) = child.id()
    {
        forwarder.child(i32::try_from(pid).unwrap_or(0));
    }
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut stdin = child.stdin.take();
    let transaction_ack = PreparedRefAck::new();
    let stdout_decoder = if transaction.is_some() {
        Some(FrameDelivery::Raw(&transaction_ack))
    } else {
        options.stdout_decoder.map(FrameDelivery::Raw)
    };
    // Credentials handed to the child can be echoed in its output. Register
    // them with the default mask as well as the host's ambient values, so a
    // capture site never has to rebuild a redactor from its own environment.
    let _credential_scope = if provenant_redact::mode().masks_egress() {
        let environment = options
            .env
            .unwrap_or_default()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_encoded_bytes().to_vec(),
                    value.as_encoded_bytes().to_vec(),
                )
            })
            .collect::<Vec<_>>();
        provenant_redact::launch_values(&environment, None)
            .ok()
            .filter(|values| !values.is_empty())
            .and_then(|values| provenant_redact::publication_scope(values).ok())
    } else {
        None
    };

    // The default mask is owned by the capture so it runs as the first filter
    // stage, ahead of any caller filter and ahead of the retention bound.
    let mut out_pump = StreamCapture::new_with_mask(
        options.stdout,
        stdout_decoder,
        Some(options.stdout_mask),
        options.stdout_filter,
        options.stdout_writer,
    )?;
    let mut err_pump = StreamCapture::new_with_mask(
        options.stderr,
        options.stderr_decoder.map(FrameDelivery::Raw),
        Some(options.stderr_mask),
        options.stderr_filter,
        options.stderr_writer,
    )?;
    let input = options.stdin.unwrap_or_default();
    let mut written = 0;
    let mut transaction_action = None;
    let mut transaction_action_written = 0;
    let mut output = ExecOutcome {
        termination: Termination::Exit(0),
        stdout: CapturedStream::default(),
        stderr: CapturedStream::default(),
        timed_out: false,
        drain_truncated: false,
        overflow_stopped: false,
        cancelled: false,
        elapsed_micros: 0,
    };
    let mut status = None;
    let mut drain_deadline = None;
    let mut term_at = None;
    let mut kill_at = None;
    loop {
        // Exit observation wins over cancellation and wall expiry in this turn.
        if status.is_none() {
            status = match child.observe() {
                Ok(status) => status,
                Err(error) => {
                    child.fail_and_transfer();
                    return Err(io_error("observe")(error));
                }
            };
            if status.is_some() {
                drain_deadline = Instant::now().checked_add(options.reap_timeout);
            }
        }
        out_pump.read(&mut stdout);
        err_pump.read(&mut stderr);
        if status.is_none()
            && transaction_action.is_none()
            && transaction_ack.observed()
            && let Some(transaction) = transaction.as_mut()
        {
            let cancelled = options.cancel.is_some_and(CancelToken::is_cancelled);
            let requested = if !cancelled && Instant::now() < deadline {
                (transaction.commit_after_prepare)()
            } else {
                false
            };
            let cancelled = options.cancel.is_some_and(CancelToken::is_cancelled);
            transaction_action = Some(prepared_ref_commit_allowed(
                requested,
                cancelled,
                Instant::now() < deadline,
            ));
        }

        let (pending_input, pending_written, close_when_written) = match transaction.as_ref() {
            Some(transaction) => match transaction_action {
                None => (transaction.commands_through_prepare, &mut written, false),
                Some(commit) => (
                    if commit {
                        &b"commit\n"[..]
                    } else {
                        &b"abort\n"[..]
                    },
                    &mut transaction_action_written,
                    true,
                ),
            },
            None => (input, &mut written, true),
        };
        let remaining_input = &pending_input[*pending_written..];
        if close_when_written && remaining_input.is_empty() || status.is_some() {
            stdin = None;
        }
        if !remaining_input.is_empty()
            && let Some(pipe) = stdin.as_mut()
        {
            match pipe.write(&remaining_input[..remaining_input.len().min(16 * 1024)]) {
                Ok(0) => stdin = None,
                Ok(n) => *pending_written += n,
                Err(error) if matches!(error.kind(), io::ErrorKind::BrokenPipe) => stdin = None,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => stdin = None,
            }
        }
        let now = Instant::now();
        if let Some(termination) = status {
            let finished = stdout.is_none() && stderr.is_none();
            let final_deadline = drain_deadline.unwrap_or(now);
            if finished || now >= final_deadline {
                output.termination = termination;
                output.drain_truncated = !finished;
                output.stdout = out_pump.into_outcome();
                output.stderr = err_pump.into_outcome();
                output.elapsed_micros =
                    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                return Ok(output);
            }
        }
        let overflow = options.stop_on_output_limit
            && (out_pump.outcome().truncated || err_pump.outcome().truncated);
        let forwarded_signal = signal_forwarder
            .as_ref()
            .is_some_and(crate::sys::build_group::Forwarder::signal_received);
        let cancelled = options.cancel.is_some_and(CancelToken::is_cancelled) || forwarded_signal;
        if !options.wait_for_completion
            && status.is_none()
            && term_at.is_none()
            && (now >= deadline || overflow || cancelled)
        {
            stdin = None;
            let delivered = if forwarded_signal {
                true
            } else {
                match child.signal(if forward_signals {
                    Signal::Terminate
                } else if options.process_group {
                    Signal::Kill
                } else {
                    Signal::Terminate
                }) {
                    Ok(delivered) => delivered,
                    Err(error) => {
                        child.fail_and_transfer();
                        return Err(io_error("SIGTERM")(error));
                    }
                }
            };
            output.timed_out = delivered && now >= deadline;
            output.overflow_stopped = delivered && overflow;
            output.cancelled = delivered && cancelled;
            term_at = Some(now);
        }
        if status.is_none()
            && kill_at.is_none()
            && term_at.is_some_and(|at| now.duration_since(at) >= options.terminate_grace)
        {
            if let Err(error) = child.signal(Signal::Kill) {
                child.fail_and_transfer();
                return Err(io_error("SIGKILL")(error));
            }
            kill_at = Some(now);
        }
        if kill_at.is_some_and(|at| now.duration_since(at) >= options.reap_timeout) {
            // RawChild::drop transfers its pre-reserved slot without signalling.
            return Err(ExecError::CleanupPending);
        }
        let stdin_ready = if transaction.is_some() && transaction_action.is_none() {
            None
        } else {
            stdin.as_ref()
        };
        if let Err(error) = child.ready(stdout.as_ref(), stderr.as_ref(), stdin_ready) {
            child.fail_and_transfer();
            return Err(io_error("poll")(error));
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextOutcome {
    pub outcome: ExecOutcome,
    pub stdout: String,
    pub stderr: String,
}
#[derive(Debug, Error)]
pub enum TextError {
    #[error(transparent)]
    Execution(#[from] ExecError),
    #[error("{stream} is not valid UTF-8")]
    Utf8 {
        stream: &'static str,
        #[source]
        source: std::string::FromUtf8Error,
    },
}
pub fn run_text(
    executor: &dyn Executor,
    options: ExecOptions<'_>,
) -> Result<TextOutcome, TextError> {
    let outcome = executor.execute(options)?;
    let stdout =
        String::from_utf8(outcome.stdout.bytes.clone()).map_err(|source| TextError::Utf8 {
            stream: "stdout",
            source,
        })?;
    let stderr =
        String::from_utf8(outcome.stderr.bytes.clone()).map_err(|source| TextError::Utf8 {
            stream: "stderr",
            source,
        })?;
    Ok(TextOutcome {
        outcome,
        stdout,
        stderr,
    })
}
#[cfg(test)]
mod tests;
