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

/// The process edge must keep this policy fixed and never independently reap
/// owned children. A blocked SIGCHLD is safe: polling still consumes status.
pub(crate) fn verify_child_wait_policy() -> io::Result<()> {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    // SAFETY: a null input only queries SIGCHLD. The output points to writable,
    // correctly aligned storage; libc retains no pointer and changes no action.
    // Concurrent queries are independent. The process owner must serialise any
    // disposition changes against child custody (documented at SystemExecutor).
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful sigaction initialises the complete output record.
    let action = unsafe { action.assume_init() };
    if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "child custody requires SIGCHLD without SIG_IGN or SA_NOCLDWAIT",
        ));
    }
    Ok(())
}

pub(crate) struct OwnedPid {
    child: Option<Child>,
    permit: Option<Permit>,
    reaped: bool,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
}
pub(crate) struct SpawnSpec<'a> {
    pub(crate) argv: &'a [OsString],
    pub(crate) env: Option<&'a [(OsString, OsString)]>,
    pub(crate) cwd: &'a Path,
    pub(crate) input: bool,
    pub(crate) capture_stdout: bool,
    pub(crate) capture_stderr: bool,
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
        Self::spawn_with_setup(
            permit,
            SpawnSpec {
                argv,
                env,
                cwd,
                input,
                capture_stdout,
                capture_stderr,
            },
            |result| {
                if let Some(pipe) = &result.stdin {
                    nonblocking(pipe)?;
                }
                if let Some(pipe) = &result.stdout {
                    nonblocking(pipe)?;
                }
                if let Some(pipe) = &result.stderr {
                    nonblocking(pipe)?;
                }
                Ok(())
            },
        )
    }
    pub(crate) fn spawn_with_setup(
        permit: Permit,
        spec: SpawnSpec<'_>,
        setup: impl FnOnce(&mut Self) -> io::Result<()>,
    ) -> io::Result<Self> {
        let (program, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(spec.cwd)
            .stdin(if spec.input {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(if spec.capture_stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(if spec.capture_stderr {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        if let Some(env) = spec.env {
            command.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        }
        super::child::configure_command(&mut command, &[], spec.env, false)?;
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
        if let Err(error) = setup(&mut result) {
            result.kill_before_transfer();
            return Err(error);
        }
        Ok(result)
    }
    fn kill_direct(&self) {
        if self.reaped {
            return;
        }
        let Some(child) = self.child.as_ref() else {
            return;
        };
        // SAFETY: the Child handle retains this unreaped PID, so it cannot be
        // reused while the signal is sent. The direct child was just spawned
        // by this executor. kill borrows no memory and reports through errno.
        if unsafe { libc::kill(child.id() as i32, libc::SIGKILL) } == 0 {
            return;
        }
        // ESRCH means it has already exited; any other failure remains owned
        // by the reaper after transfer. Preserve the original operation error.
        let _kill_error = io::Error::last_os_error();
    }
    fn kill_before_transfer(&mut self) {
        self.kill_direct();
        self.transfer_to_reaper();
    }
    fn transfer_to_reaper(&mut self) {
        if self.reaped {
            self.child.take();
            self.permit.take();
            return;
        }
        if let (Some(child), Some(permit)) = (self.child.take(), self.permit.take()) {
            permit.transfer(child);
        }
    }
    pub(crate) fn fail_and_transfer(mut self) {
        self.kill_before_transfer();
    }
    #[cfg(test)]
    pub(crate) fn pid_for_test(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
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
        if !self.reaped {
            self.transfer_to_reaper();
        }
    }
}

#[cfg(all(test, feature = "fixture"))]
pub(crate) fn reap_pid_for_test(pid: u32) -> io::Result<i32> {
    let mut status = 0;
    // SAFETY: callers pass the PID returned by this test process's own spawn;
    // the fixture pauses its reaper before intentionally taking the wait.
    let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
    if waited == pid as libc::pid_t {
        Ok(status)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(test, feature = "fixture"))]
pub(crate) fn pid_is_absent_for_test(pid: u32) -> io::Result<bool> {
    // SAFETY: signal zero only checks the status of the just-reaped fixture
    // PID and sends no signal; no pointer or ownership crosses the call.
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(true)
    } else {
        Err(error)
    }
}
