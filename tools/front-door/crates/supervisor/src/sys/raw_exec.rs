//! Direct-child utility custody; all descriptor flags and signals stay here.
use crate::exec::Termination;
use crate::identity::Signal;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) struct RawChild {
    child: Child,
    reaped: bool,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
}
fn nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: fd is borrowed and live for both calls; flags belong to this
    // private pipe description. No pointer, ownership or shared state escapes.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
impl RawChild {
    pub(crate) fn spawn(
        argv: &[OsString],
        env: Option<&[(OsString, OsString)]>,
        cwd: &Path,
        input: bool,
        capture_stdout: bool,
        capture_stderr: bool,
    ) -> io::Result<Self> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(if input { Stdio::piped() } else { Stdio::null() })
            .stdout(if capture_stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(if capture_stderr {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        if let Some(env) = env {
            command.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        }
        super::child::configure_command(&mut command, &[], env, false)?;
        let child = command.spawn()?;
        let mut result = Self {
            child,
            reaped: false,
            stdin: None,
            stdout: None,
            stderr: None,
        };
        result.stdin = result.child.stdin.take();
        result.stdout = result.child.stdout.take();
        result.stderr = result.child.stderr.take();
        if let Some(pipe) = &result.stdin {
            nonblocking(pipe)?;
        }
        if let Some(pipe) = &result.stdout {
            nonblocking(pipe)?;
        }
        if let Some(pipe) = &result.stderr {
            nonblocking(pipe)?;
        }
        Ok(result)
    }
    pub(crate) fn observe(&mut self) -> io::Result<Option<Termination>> {
        self.child.try_wait().map(|status| {
            status.map(|status| {
                self.reaped = true;
                match status.code() {
                    Some(code) => Termination::Exit(code),
                    None => Termination::Signal(status.signal().unwrap_or(0)),
                }
            })
        })
    }
    pub(crate) fn signal(&mut self, signal: Signal) -> io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        let sig = match signal {
            Signal::Terminate => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: this unreaped direct child reserves its PID. No other handle
        // can wait it through this API; concurrent OS death leaves a zombie,
        // never a recycled PID. kill borrows no memory, errno is thread-local.
        if unsafe { libc::kill(self.child.id() as i32, sig) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
    pub(crate) fn ready(
        &self,
        out: Option<&ChildStdout>,
        err: Option<&ChildStderr>,
        input: Option<&ChildStdin>,
    ) -> io::Result<bool> {
        let mut fds = Vec::with_capacity(3);
        for fd in out
            .map(AsRawFd::as_raw_fd)
            .into_iter()
            .chain(err.map(AsRawFd::as_raw_fd))
        {
            fds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        if let Some(input) = input {
            fds.push(libc::pollfd {
                fd: input.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            });
        }
        // SAFETY: poll borrows a private writable array of live borrowed pipe
        // descriptors, with exact count. It transfers no ownership; timeout is
        // at most 5ms, EINTR is retried by the outer bounded loop.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 5) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(result > 0)
    }
}
impl Drop for RawChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.signal(Signal::Kill);
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                if self.observe().ok().flatten().is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}
