//! Bounded byte-preserving utility execution, separate from S2 provider custody.
//! No detached readers or writers: one owner pumps all three nonblocking pipes.
use crate::sys::raw_exec::RawChild;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    Exit(i32),
    Signal(i32),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedStream {
    pub bytes: Vec<u8>,
    pub total_bytes: u64,
    pub truncated: bool,
}
impl CapturedStream {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            total_bytes: 0,
            truncated: false,
        }
    }
    fn append(&mut self, bytes: &[u8], limit: usize) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len() as u64);
        let room = limit.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
        self.truncated |= bytes.len() > room;
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecOutcome {
    pub termination: Termination,
    pub stdout: CapturedStream,
    pub stderr: CapturedStream,
    pub timed_out: bool,
}
/// Environment is a complete replacement when Some, inherited when None.
/// Limits bound retained bytes; excess bytes are drained and optionally forwarded.
/// A sink must return promptly: arbitrary caller I/O cannot be preempted safely.
/// A deadline covers child exit and pipe EOF; inherited descendant pipes are
/// closed at the deadline after the direct child exits. No descendant is signalled.
pub struct ExecOptions<'a> {
    pub argv: &'a [OsString],
    pub env: Option<&'a [(OsString, OsString)]>,
    pub cwd: &'a Path,
    pub stdin: Option<&'a [u8]>,
    pub stdout_limit: usize,
    pub stderr_limit: usize,
    pub discard_stdout: bool,
    pub discard_stderr: bool,
    pub stdout_sink: Option<&'a mut dyn Write>,
    pub stderr_sink: Option<&'a mut dyn Write>,
    pub timeout: Duration,
    pub terminate_grace: Duration,
    pub reap_timeout: Duration,
    /// Stop on overflow instead of draining until normal completion.
    pub stop_on_output_limit: bool,
}
impl<'a> ExecOptions<'a> {
    pub fn new(argv: &'a [OsString], cwd: &'a Path, timeout: Duration) -> Self {
        Self {
            argv,
            cwd,
            timeout,
            env: None,
            stdin: None,
            stdout_limit: 1_048_576,
            stderr_limit: 1_048_576,
            discard_stdout: false,
            discard_stderr: false,
            stdout_sink: None,
            stderr_sink: None,
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
    #[error("execution duration is outside the supported clock range")]
    DurationOutOfRange,
    #[error("utility {operation} failed")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("utility child did not exit after SIGKILL within {timeout:?}")]
    ReapTimeout { timeout: Duration },
}
fn io_error(operation: &'static str) -> impl FnOnce(io::Error) -> ExecError {
    move |source| ExecError::Io { operation, source }
}
/// The executor seam permits fake outcomes without processes or global state.
pub trait Executor: Send + Sync {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemExecutor;
impl Executor for SystemExecutor {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
        run(options)
    }
}

fn pump<R: Read>(
    pipe: &mut Option<R>,
    capture: &mut CapturedStream,
    limit: usize,
    sink: &mut Option<&mut dyn Write>,
) -> Result<(), ExecError> {
    let Some(reader) = pipe else { return Ok(()) };
    // One bounded chunk per turn ensures a continuously writing child cannot
    // starve stderr, stdin, exit observation, or the wall-clock deadline.
    let mut buffer = [0; 16 * 1024];
    match reader.read(&mut buffer) {
        Ok(0) => *pipe = None,
        Ok(count) => {
            capture.append(&buffer[..count], limit);
            if let Some(sink) = sink {
                sink.write_all(&buffer[..count])
                    .map_err(io_error("forward"))?;
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) => {}
        Err(error) => return Err(io_error("read pipe")(error)),
    }
    Ok(())
}
/// TERM on deadline, KILL after grace, then a bounded final reap. Only the
/// direct utility child created here can be signalled; never a provider PID.
pub fn run(mut options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
    if options.argv.is_empty() {
        return Err(ExecError::EmptyArgv);
    }
    let started = Instant::now();
    let deadline = started
        .checked_add(options.timeout)
        .ok_or(ExecError::DurationOutOfRange)?;
    // Validate all deadline arithmetic before creating a process.
    deadline
        .checked_add(options.terminate_grace)
        .and_then(|v| v.checked_add(options.reap_timeout))
        .ok_or(ExecError::DurationOutOfRange)?;
    let mut child = RawChild::spawn(
        options.argv,
        options.env,
        options.cwd,
        options.stdin.is_some(),
        !options.discard_stdout,
        !options.discard_stderr,
    )
    .map_err(io_error("spawn"))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut stdin = child.stdin.take();
    let input = options.stdin.unwrap_or_default();
    let mut written = 0;
    let mut output = ExecOutcome {
        termination: Termination::Exit(0),
        stdout: CapturedStream::new(),
        stderr: CapturedStream::new(),
        timed_out: false,
    };
    let mut status = None;
    let mut term_at = None;
    let mut kill_at = None;
    loop {
        pump(
            &mut stdout,
            &mut output.stdout,
            options.stdout_limit,
            &mut options.stdout_sink,
        )?;
        pump(
            &mut stderr,
            &mut output.stderr,
            options.stderr_limit,
            &mut options.stderr_sink,
        )?;
        if written == input.len() {
            stdin = None;
        }
        if let Some(pipe) = stdin.as_mut() {
            match pipe.write(&input[written..input.len().min(written.saturating_add(16 * 1024))]) {
                Ok(0) => stdin = None,
                Ok(n) => written += n,
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => stdin = None,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(io_error("write stdin")(error)),
            }
        }
        if status.is_none() {
            status = child.observe().map_err(io_error("wait"))?;
        }
        let now = Instant::now();
        output.timed_out |= now >= deadline;
        if let Some(termination) = status {
            if stdout.is_none() && stderr.is_none() {
                output.termination = termination;
                return Ok(output);
            }
        }
        let overflow =
            options.stop_on_output_limit && (output.stdout.truncated || output.stderr.truncated);
        if term_at.is_none() && (now >= deadline || overflow) {
            output.timed_out = now >= deadline;
            stdin = None;
            if let Some(termination) = status {
                // The child was reaped; its PID must never be signalled again.
                output.termination = termination;
                return Ok(output);
            }
            child
                .signal(crate::identity::Signal::Terminate)
                .map_err(io_error("SIGTERM"))?;
            term_at = Some(now);
        }
        if kill_at.is_none()
            && term_at.is_some_and(|at| now.duration_since(at) >= options.terminate_grace)
        {
            if status.is_none() {
                child
                    .signal(crate::identity::Signal::Kill)
                    .map_err(io_error("SIGKILL"))?;
            }
            kill_at = Some(now);
        }
        if kill_at.is_some_and(|at| now.duration_since(at) >= options.reap_timeout) {
            if let Some(termination) = status {
                output.termination = termination;
                return Ok(output);
            }
            return Err(ExecError::ReapTimeout {
                timeout: options.reap_timeout,
            });
        }
        // Polling backoff is bounded; output-heavy children get the next turn
        // immediately so a fixed sleep does not impose a capture throughput cap.
        if !child
            .ready(stdout.as_ref(), stderr.as_ref(), stdin.as_ref())
            .map_err(io_error("poll"))?
        {
            std::thread::yield_now();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextOutcome {
    pub termination: Termination,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
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
/// Strict UTF-8 only: no newline translation or replacement characters.
pub fn run_text(
    executor: &dyn Executor,
    options: ExecOptions<'_>,
) -> Result<TextOutcome, TextError> {
    let result = executor.execute(options)?;
    let stdout = String::from_utf8(result.stdout.bytes).map_err(|source| TextError::Utf8 {
        stream: "stdout",
        source,
    })?;
    let stderr = String::from_utf8(result.stderr.bytes).map_err(|source| TextError::Utf8 {
        stream: "stderr",
        source,
    })?;
    Ok(TextOutcome {
        termination: result.termination,
        stdout,
        stderr,
        stdout_truncated: result.stdout.truncated,
        stderr_truncated: result.stderr.truncated,
        timed_out: result.timed_out,
    })
}
#[cfg(test)]
mod tests;
