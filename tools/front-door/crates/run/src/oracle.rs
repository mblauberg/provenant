//! One interpreter gate for every run differential.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(15);
const REAP_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_CAPTURE_BYTES: u64 = 4 * 1024 * 1024;

/// Runs a command with file-backed output and a bounded child wait.
///
/// File-backed capture avoids waiting for EOF from inherited pipe descriptors
/// after the direct child exits. Captures are also capped before allocation.
pub(crate) trait CommandOutput {
    fn bounded_output(&mut self) -> io::Result<Output>;
}

impl CommandOutput for Command {
    fn bounded_output(&mut self) -> io::Result<Output> {
        let mut stdout_file = tempfile::tempfile()?;
        let mut stderr_file = tempfile::tempfile()?;
        self.stdout(Stdio::from(stdout_file.try_clone()?))
            .stderr(Stdio::from(stderr_file.try_clone()?));
        let mut child = self.spawn()?;
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::yield_now(),
                Ok(None) => {
                    kill_and_reap(&mut child);
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "child exceeded its 15 second bound",
                    ));
                }
                Err(error) => {
                    kill_and_reap(&mut child);
                    return Err(error);
                }
            }
        };
        let stdout = read_capture(&mut stdout_file)?;
        let stderr = read_capture(&mut stderr_file)?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

fn read_capture(file: &mut File) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(MAX_CAPTURE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CAPTURE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "child output exceeded the 4 MiB capture bound",
        ));
    }
    Ok(bytes)
}

fn kill_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let deadline = Instant::now() + REAP_TIMEOUT;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::yield_now();
    }
}

fn isolated(interpreter: &std::ffi::OsStr) -> Command {
    let mut command = Command::new(interpreter);
    command
        .arg("-I")
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .env("LC_ALL", "C.UTF-8");
    command
}

fn unavailable(reason: impl std::fmt::Display) -> Option<Command> {
    assert!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() != Ok("1"),
        "required Python differential unavailable: {reason}"
    );
    eprintln!("skip: {reason}");
    None
}

pub(crate) fn python() -> Option<Command> {
    let interpreter = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let probe = match isolated(&interpreter)
        .args([
            "-c",
            "import sys;sys.exit(0 if sys.version_info >= (3,12) else 1)",
        ])
        .bounded_output()
    {
        Ok(output) => output,
        Err(error) => return unavailable(format_args!("{interpreter:?}: {error}")),
    };
    if !probe.status.success() {
        return unavailable(format_args!("{interpreter:?} requires Python >=3.12"));
    }
    Some(isolated(&interpreter))
}
