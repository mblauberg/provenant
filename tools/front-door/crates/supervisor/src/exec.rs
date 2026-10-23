//! Bounded direct-child utility execution. Provider custody belongs to S2.
use crate::identity::Signal;
use crate::sys::{raw_exec::OwnedPid as RawChild, reaper::Reaper};
use provenant_core::CancelToken;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
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
pub struct TeeQueue(SyncSender<Vec<u8>>);
impl NonblockingSink for TeeQueue {
    fn try_send(&self, bytes: &[u8]) -> Result<(), QueueLoss> {
        self.0
            .try_send(bytes.to_vec())
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => QueueLoss::Full,
                mpsc::TrySendError::Disconnected(_) => QueueLoss::Closed,
            })
    }
}
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
    fn limit(&self) -> usize {
        match self {
            Self::Capture { limit } | Self::Tee { limit, .. } => *limit,
            Self::Discard => 0,
        }
    }
    fn piped(&self) -> bool {
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
}
pub struct ExecOptions<'a> {
    pub argv: &'a [OsString],
    pub env: Option<&'a [(OsString, OsString)]>,
    pub cwd: &'a Path,
    pub stdin: Option<&'a [u8]>,
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
    #[error("utility cleanup admission is full")]
    Busy,
    #[error("utility cancelled before spawn")]
    Cancelled,
    #[error("utility cleanup transferred to the reserved reaper")]
    CleanupPending,
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
impl Executor for SystemExecutor {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
        run(&self.reaper, options)
    }
}
#[derive(Default)]
struct Frames {
    bytes: Vec<u8>,
    oversized: bool,
}
impl Frames {
    fn decode(&mut self, chunk: &[u8], sink: Option<&dyn FrameSink>, out: &mut CapturedStream) {
        let Some(sink) = sink else {
            return;
        };
        for &byte in chunk {
            if byte == b'\n' {
                if !self.oversized && sink.try_frame(&self.bytes).is_err() {
                    out.decoder_lost_frames = out.decoder_lost_frames.saturating_add(1);
                }
                self.bytes.clear();
                self.oversized = false;
            } else if !self.oversized {
                if self.bytes.len() == FRAME_LIMIT {
                    self.bytes.clear();
                    self.oversized = true;
                    out.oversized_frames = out.oversized_frames.saturating_add(1);
                } else {
                    self.bytes.push(byte);
                }
            }
        }
    }
    fn finish(&mut self, sink: Option<&dyn FrameSink>, out: &mut CapturedStream) {
        if !self.bytes.is_empty()
            && !self.oversized
            && sink.is_some_and(|sink| sink.try_frame(&self.bytes).is_err())
        {
            out.decoder_lost_frames = out.decoder_lost_frames.saturating_add(1);
        }
        self.bytes.clear();
    }
}
struct Pump<'a> {
    policy: StreamPolicy<'a>,
    decoder: Option<&'a dyn FrameSink>,
    filter: Option<&'a mut dyn CaptureFilter>,
    frames: Frames,
}
impl Pump<'_> {
    fn read<R: Read>(&mut self, pipe: &mut Option<R>, capture: &mut CapturedStream) {
        let Some(reader) = pipe else {
            return;
        };
        let mut buffer = [0; 16 * 1024];
        match reader.read(&mut buffer) {
            Ok(0) => {
                self.frames.finish(self.decoder, capture);
                *pipe = None;
            }
            Ok(count) => {
                let bytes = &buffer[..count];
                capture.total_bytes = capture.total_bytes.saturating_add(count as u64);
                self.frames.decode(bytes, self.decoder, capture);
                let filtered;
                let bytes = if capture.filter_failed {
                    &[][..]
                } else if let Some(filter) = self.filter.as_mut() {
                    match filter.filter(bytes) {
                        Ok(value) if value.len() <= FRAME_LIMIT => {
                            filtered = value;
                            &filtered
                        }
                        _ => {
                            capture.filter_failed = true;
                            &[][..]
                        }
                    }
                } else {
                    bytes
                };
                let room = self.policy.limit().saturating_sub(capture.bytes.len());
                capture
                    .bytes
                    .extend_from_slice(&bytes[..bytes.len().min(room)]);
                capture.truncated |= bytes.len() > room;
                if let StreamPolicy::Tee { sink, .. } = self.policy {
                    if let Err(loss) = sink.try_send(bytes) {
                        capture.sink_lost_bytes =
                            capture.sink_lost_bytes.saturating_add(bytes.len() as u64);
                        capture.sink_failed |= loss == QueueLoss::Closed;
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => {
                capture.read_failed = true;
                *pipe = None;
            }
        }
    }
}
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
        options.argv,
        options.env,
        options.cwd,
        options.stdin.is_some(),
        options.stdout.piped(),
        options.stderr.piped(),
    )
    .map_err(io_error("spawn"))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut stdin = child.stdin.take();
    let mut out_pump = Pump {
        policy: options.stdout,
        decoder: options.stdout_decoder,
        filter: options.stdout_filter,
        frames: Frames::default(),
    };
    let mut err_pump = Pump {
        policy: options.stderr,
        decoder: options.stderr_decoder,
        filter: options.stderr_filter,
        frames: Frames::default(),
    };
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
    let mut term_at = None;
    let mut kill_at = None;
    loop {
        // Exit observation wins over cancellation and wall expiry in this turn.
        if status.is_none() {
            status = child.observe().map_err(io_error("observe"))?;
        }
        out_pump.read(&mut stdout, &mut output.stdout);
        err_pump.read(&mut stderr, &mut output.stderr);
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
            let final_deadline = kill_at
                .map(|at: Instant| at + options.reap_timeout)
                .unwrap_or(deadline);
            if finished || now >= final_deadline {
                output.termination = termination;
                output.drain_truncated = !finished;
                output.elapsed_micros =
                    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                return Ok(output);
            }
        }
        let overflow =
            options.stop_on_output_limit && (output.stdout.truncated || output.stderr.truncated);
        let cancelled = options.cancel.is_some_and(CancelToken::is_cancelled);
        if status.is_none() && term_at.is_none() && (now >= deadline || overflow || cancelled) {
            stdin = None;
            let delivered = child
                .signal(Signal::Terminate)
                .map_err(io_error("SIGTERM"))?;
            output.timed_out = delivered && now >= deadline;
            output.overflow_stopped = delivered && overflow;
            output.cancelled = delivered && cancelled;
            term_at = Some(now);
        }
        if status.is_none()
            && kill_at.is_none()
            && term_at.is_some_and(|at| now.duration_since(at) >= options.terminate_grace)
        {
            child.signal(Signal::Kill).map_err(io_error("SIGKILL"))?;
            kill_at = Some(now);
        }
        if kill_at.is_some_and(|at| now.duration_since(at) >= options.reap_timeout) {
            // RawChild::drop transfers its pre-reserved slot without signalling.
            return Err(ExecError::CleanupPending);
        }
        child
            .ready(stdout.as_ref(), stderr.as_ref(), stdin.as_ref())
            .map_err(io_error("poll"))?;
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
