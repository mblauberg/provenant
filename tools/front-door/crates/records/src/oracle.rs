//! One interpreter gate for every records differential.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

pub(crate) struct CapturedOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

pub(crate) struct BoundedChild {
    child: Child,
    stdout: File,
    stderr: File,
}

impl BoundedChild {
    pub(crate) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub(crate) fn wait(mut self) -> io::Result<CapturedOutput> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let reap_deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < reap_deadline {
                    if self.child.try_wait()?.is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "Python differential timed out",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Python differential could not be reaped promptly",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Ok(CapturedOutput {
            status,
            stdout: read_file(&mut self.stdout)?,
            stderr: read_file(&mut self.stderr)?,
        })
    }
}

const MAX_CAPTURE_BYTES: u64 = 4 * 1024 * 1024;

fn read_file(file: &mut File) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut contents = Vec::new();
    (&mut *file)
        .take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_CAPTURE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Python differential output exceeded the 4 MiB capture limit",
        ));
    }
    Ok(contents)
}

pub(crate) fn spawn(command: &mut Command) -> io::Result<BoundedChild> {
    let stdout = tempfile::tempfile()?;
    let stderr = tempfile::tempfile()?;
    command
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?));
    let child = command.spawn()?;
    Ok(BoundedChild {
        child,
        stdout,
        stderr,
    })
}

pub(crate) fn run(command: &mut Command) -> io::Result<CapturedOutput> {
    spawn(command)?.wait()
}

fn isolated(interpreter: &std::ffi::OsStr) -> Command {
    let mut command = Command::new(interpreter);
    command
        .stdin(Stdio::null())
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
    let mut command = isolated(&interpreter);
    command.args([
        "-c",
        "import sys;sys.exit(0 if sys.version_info >= (3,12) else 1)",
    ]);
    let probe = match run(&mut command) {
        Ok(output) => output,
        Err(error) => return unavailable(format_args!("{interpreter:?}: {error}")),
    };
    if !probe.status.success() {
        return unavailable(format_args!("{interpreter:?} requires Python >=3.12"));
    }
    Some(isolated(&interpreter))
}
