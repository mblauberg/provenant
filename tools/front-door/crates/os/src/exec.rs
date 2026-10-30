//! Bounded direct-child utility execution. Provider custody belongs to S2.
use crate::capture::{FrameDelivery, StreamCapture};
#[cfg(test)]
pub(crate) use crate::capture::{Frames, Pump};
use crate::identity::Signal;
use crate::sys::{raw_exec::OwnedPid as RawChild, reaper::Reaper, resources::ProcessBudget};
use provenant_core::CancelToken;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
#[cfg(test)]
use std::io::Read;
use std::io::{self, Write};
use std::path::Path;
#[cfg(any(test, feature = "fixture"))]
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};
use thiserror::Error;

pub const DEFAULT_CAPTURE_LIMIT: usize = 8 * 1024 * 1024;
pub const MAX_CAPTURE_LIMIT: usize = 64 * 1024 * 1024;
pub const FRAME_LIMIT: usize = 64 * 1024;
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
            Self::Discard => 0,
        }
    }
    pub(crate) fn piped(&self) -> bool {
        !matches!(self, Self::Discard)
    }
}
/// Frames are decoded before retention and redaction; full queues are loss,
/// never a synthetic terminal or a process stop request.
pub trait FrameSink: Send + Sync {
    fn try_frame(&self, bytes: &[u8]) -> Result<(), QueueLoss>;
}
#[derive(Debug, Error)]
#[error("capture filter failed")]
pub struct FilterError;
pub trait CaptureFilter {
    fn filter(&mut self, bytes: &[u8]) -> Result<Vec<u8>, FilterError>;

    /// Flush bytes held until the end of the stream. Filters without buffered
    /// state can keep the default implementation.
    fn finish(&mut self) -> Result<Vec<u8>, FilterError> {
        Ok(Vec::new())
    }
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

pub struct ExecOptions<'a> {
    pub argv: &'a [OsString],
    pub env: Option<&'a [(OsString, OsString)]>,
    pub cwd: &'a Path,
    pub stdin: Option<&'a [u8]>,
    pub secret_input: Option<&'a SecretInput>,
    /// Pass a lane or updater lease through exec, including to descendants.
    pub inherited_lease: Option<&'a crate::fs::LockGuard>,
    /// Run in a private session and kill the whole group immediately on timeout.
    pub process_group: bool,
    pub stdout: StreamPolicy<'a>,
    pub stderr: StreamPolicy<'a>,
    pub stdout_decoder: Option<&'a dyn FrameSink>,
    pub stderr_decoder: Option<&'a dyn FrameSink>,
    pub stdout_filter: Option<&'a mut dyn CaptureFilter>,
    pub stderr_filter: Option<&'a mut dyn CaptureFilter>,
    pub cancel: Option<&'a CancelToken>,
    pub timeout: Duration,
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
            process_group: false,
            stdout: StreamPolicy::default(),
            stderr: StreamPolicy::default(),
            stdout_decoder: None,
            stderr_decoder: None,
            stdout_filter: None,
            stderr_filter: None,
            cancel: None,
            timeout,
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
}
fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> ExecError {
    move |source| ExecError::Io { operation, source }
}
pub trait Executor: Send + Sync {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError>;
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

/// Cargo places test executables, and only those, in a profile's `deps`.
fn is_test_harness(program: &Path) -> bool {
    program
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|directory| directory == "deps")
}

impl Executor for SystemExecutor {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
        run(&self.reaper, options)
    }
}
// Process supervision deadlines use the operating system's monotonic clock.
#[allow(clippy::disallowed_methods)]
fn run(reaper: &Reaper, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
    if options.argv.is_empty() {
        return Err(ExecError::EmptyArgv);
    }
    if options.stdout.limit() > MAX_CAPTURE_LIMIT || options.stderr.limit() > MAX_CAPTURE_LIMIT {
        return Err(ExecError::InvalidLimit);
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
    let mut child = RawChild::spawn(
        permit,
        crate::sys::raw_exec::SpawnSpec {
            argv: options.argv,
            env: options.env,
            cwd: options.cwd,
            input: options.stdin.is_some(),
            secret_input: options.secret_input,
            inherited_lease: options.inherited_lease,
            inherited_ports: None,
            process_group: options.process_group,
            capture_stdout: options.stdout.piped(),
            capture_stderr: options.stderr.piped(),
        },
    )
    .map_err(io_error("spawn"))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut stdin = child.stdin.take();
    let mut out_pump = StreamCapture::new(
        options.stdout,
        options.stdout_decoder.map(FrameDelivery::Raw),
        options.stdout_filter,
    )?;
    let mut err_pump = StreamCapture::new(
        options.stderr,
        options.stderr_decoder.map(FrameDelivery::Raw),
        options.stderr_filter,
    )?;
    let input = options.stdin.unwrap_or_default();
    let mut written = 0;
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
        if written == input.len() || status.is_some() {
            stdin = None;
        }
        if let Some(pipe) = stdin.as_mut() {
            match pipe.write(&input[written..input.len().min(written.saturating_add(16 * 1024))]) {
                Ok(0) => stdin = None,
                Ok(n) => written += n,
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
        let cancelled = options.cancel.is_some_and(CancelToken::is_cancelled);
        if status.is_none() && term_at.is_none() && (now >= deadline || overflow || cancelled) {
            stdin = None;
            let delivered = match child.signal(if options.process_group {
                Signal::Kill
            } else {
                Signal::Terminate
            }) {
                Ok(delivered) => delivered,
                Err(error) => {
                    child.fail_and_transfer();
                    return Err(io_error("SIGTERM")(error));
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
        if let Err(error) = child.ready(stdout.as_ref(), stderr.as_ref(), stdin.as_ref()) {
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
