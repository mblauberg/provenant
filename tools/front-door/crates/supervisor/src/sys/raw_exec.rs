//! Direct-child utility custody; all descriptor flags and signals stay here.
use super::reaper::Permit;
use crate::exec::Termination;
use crate::identity::Signal;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

pub struct OwnedPid {
    child: Option<Child>,
    permit: Option<Permit>,
    reaped: bool,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
}
fn nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: fd is borrowed and live for both calls; flags belong to this
    // private pipe description. No pointer, ownership or shared state escapes.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the same live, private descriptor is borrowed; F_SETFL preserves
    // other flags and retains no pointer or ownership. Errors use thread errno.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
impl OwnedPid {
    pub(crate) fn spawn(
        permit: Permit,
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
            child: Some(child),
            permit: Some(permit),
            reaped: false,
            stdin: None,
            stdout: None,
            stderr: None,
        };
        if let Some(child) = result.child.as_mut() {
            result.stdin = child.stdin.take();
            result.stdout = child.stdout.take();
            result.stderr = child.stderr.take();
        }
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
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        child.try_wait().map(|status| {
            status.map(|status| {
                self.reaped = true;
                match status.code() {
                    Some(code) => Termination::Exit(i64::from(code)),
                    None => Termination::Signal(i64::from(status.signal().unwrap_or(0))),
                }
            })
        })
    }
    pub(crate) fn signal(&mut self, signal: Signal) -> io::Result<bool> {
        if self.reaped {
            return Ok(false);
        }
        // Re-observe immediately before delivery: an observed exit wins.
        if self.observe()?.is_some() {
            return Ok(false);
        }
        let child = self
            .child
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let sig = match signal {
            Signal::Terminate => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: this unreaped direct child reserves its PID. No other handle
        // can wait it through this API; concurrent OS death leaves a zombie,
        // never a recycled PID. kill borrows no memory, errno is thread-local.
        if unsafe { libc::kill(child.id() as i32, sig) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
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
impl Drop for OwnedPid {
    fn drop(&mut self) {
        if let (Some(child), Some(permit)) = (self.child.take(), self.permit.take())
            && !self.reaped
        {
            permit.transfer(child);
        }
    }
}
