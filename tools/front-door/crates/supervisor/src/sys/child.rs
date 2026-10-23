//! Child-only custody for the bounded utility runner, separate from S2 providers.
use crate::identity::Signal;
use std::ffi::{CString, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ChildWaitError {
    #[error("utility child {pid} did not exit within {timeout:?}")]
    Timeout { pid: u32, timeout: Duration },
    #[error("utility child wait failed")]
    Io(#[from] io::Error),
}

pub(crate) fn anonymous_spool() -> io::Result<File> {
    let mut bytes = [0_u8; 16];
    // SAFETY: getentropy writes exactly bytes.len() (below its 256-byte limit)
    // into a live exclusive allocation. No state or pointers escape; errno is
    // read on failure and concurrent calls are independent.
    if unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let suffix = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = std::env::temp_dir().join(format!("provenant-capture-{suffix}"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    Ok(file)
}

/// Private session custody for utilities. Like Popen.poll(), observation reaps
/// the direct child so its zombie cannot keep the group probe alive. S2 provider
/// custody is a separate owner and retains its WNOWAIT identity.
pub(crate) struct OwnedChild {
    child: Child,
    cleanup_complete: bool,
    group_gone: bool,
}

impl OwnedChild {
    pub(crate) fn spawn(
        command: &mut Command,
        pass_fds: &[BorrowedFd<'_>],
        env: Option<&[(OsString, OsString)]>,
    ) -> io::Result<Self> {
        configure_command(command, pass_fds, env, true)?;
        Ok(Self {
            child: command.spawn()?,
            cleanup_complete: false,
            group_gone: false,
        })
    }

    pub(crate) fn exited(&mut self) -> io::Result<bool> {
        // Child caches the reaped status. Repeated observation is safe.
        Ok(self.child.try_wait()?.is_some())
    }

    pub(crate) fn group_exists(&mut self) -> bool {
        if self.group_gone {
            return false;
        }
        // SAFETY: spawn created this private session/group. While it has any
        // member its group ID cannot be reused. Signal 0 has no effect; only
        // ESRCH proves absence. Cache absence to avoid probing a recycled ID.
        let status = unsafe { libc::kill(-(self.child.id() as i32), 0) };
        self.group_gone =
            status != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        !self.group_gone
    }

    pub(crate) fn signal(&mut self, signal: Signal) -> io::Result<()> {
        if self.group_gone {
            return Ok(());
        }
        let sig = match signal {
            Signal::Terminate => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: the child's live group reserves its session/group ID. This
        // private handle cannot target an arbitrary provider or sibling group.
        // kill takes no ownership and reports errors through thread-local errno.
        if unsafe { libc::kill(-(self.child.id() as i32), sig) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            self.group_gone = true;
            return Ok(());
        }
        Err(error)
    }

    pub(crate) fn kill_direct(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    pub(crate) fn reap(&mut self, timeout: Duration) -> Result<ExitStatus, ChildWaitError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ChildWaitError::Timeout {
                    pid: self.child.id(),
                    timeout,
                });
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }

    pub(crate) fn finish_cleanup(&mut self) {
        self.cleanup_complete = true;
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Best effort on unwinding only, with a bounded wait even if the kernel
        // cannot complete SIGKILL. No guarantee on abort or owner SIGKILL.
        if !self.cleanup_complete {
            let _ = self.signal(Signal::Kill);
            if !self.exited().unwrap_or(false) {
                let _ = self.child.kill();
            }
            let _ = self.reap(Duration::from_millis(250));
        }
    }
}

/// Install the same strict argv/PATH/env syscall boundary for all utilities.
pub(crate) fn configure_command(
    command: &mut Command,
    pass_fds: &[BorrowedFd<'_>],
    env: Option<&[(OsString, OsString)]>,
    new_session: bool,
) -> io::Result<()> {
    let pass: Vec<_> = pass_fds.iter().map(AsRawFd::as_raw_fd).collect();
    let env: Vec<_> = env
        .map(<[_]>::to_vec)
        .unwrap_or_else(|| std::env::vars_os().collect());
    let string =
        |bytes: &[u8]| CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL));
    let arguments: Vec<_> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| string(arg.as_bytes()))
        .collect::<io::Result<_>>()?;
    let environment: Vec<_> = env
        .iter()
        .map(|(key, value)| {
            let bytes = [key.as_bytes(), b"=", value.as_bytes()].concat();
            string(&bytes)
        })
        .collect::<io::Result<_>>()?;
    let program = command.get_program().as_bytes();
    let programs = if program.contains(&b'/') {
        vec![string(program)?]
    } else {
        let path = env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.as_bytes())
            .unwrap_or(b"/bin:/usr/bin");
        path.split(|&byte| byte == b':')
            .map(|dir| string(&[dir, if dir.is_empty() { b"" } else { b"/" }, program].concat()))
            .collect::<io::Result<Vec<_>>>()?
    };
    let buffers = ExecBuffers::new(arguments, environment);
    // SAFETY: this hook runs after fork, before exec. It only invokes
    // async-signal-safe syscalls over preallocated vectors, does not lock,
    // allocate or mutate parent state. Each borrowed passed fd stays live
    // through spawn. Rust's exec-error pipe is already CLOEXEC. Failures
    // return errno through that pipe and std reaps the failed child.
    unsafe {
        command.pre_exec(move || {
            if new_session && libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            cloexec_descriptors()?;
            for &fd in &pass {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let mut saved_error = None;
            let mut last_error = libc::ENOENT;
            for program in &programs {
                buffers.exec(program);
                let error = io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO);
                if !matches!(error, libc::ENOENT | libc::ENOTDIR) && saved_error.is_none() {
                    saved_error = Some(error);
                }
                last_error = error;
            }
            Err(io::Error::from_raw_os_error(
                saved_error.unwrap_or(last_error),
            ))
        });
    }
    Ok(())
}

// The pointer arrays borrow immutable CString allocations owned by this object.
// Vec moves never move those allocations, and no pointer escapes this module.
struct ExecBuffers {
    _arguments: Vec<CString>,
    _environment: Vec<CString>,
    argv: Vec<*const libc::c_char>,
    envp: Vec<*const libc::c_char>,
}
// SAFETY: every raw pointer references an immutable allocation owned by Self;
// transfer never moves the pointee, and destruction occurs after the last use.
unsafe impl Send for ExecBuffers {}
// SAFETY: all operations borrow immutable arrays and immutable CString data;
// there are no mutators, libc retains no pointers, and errno is thread-local.
unsafe impl Sync for ExecBuffers {}
impl ExecBuffers {
    fn new(arguments: Vec<CString>, environment: Vec<CString>) -> Self {
        let argv = arguments
            .iter()
            .map(|arg| arg.as_ptr())
            .chain([std::ptr::null()])
            .collect();
        let envp = environment
            .iter()
            .map(|arg| arg.as_ptr())
            .chain([std::ptr::null()])
            .collect();
        Self {
            _arguments: arguments,
            _environment: environment,
            argv,
            envp,
        }
    }
    fn exec(&self, program: &std::ffi::CStr) {
        // SAFETY: program and every argv/envp pointee are live NUL-terminated
        // strings, arrays have null terminators, and Self owns them through
        // return or process replacement. No Rust memory is mutated or handed
        // over; concurrent invocations either return errno or replace the process.
        unsafe { libc::execve(program.as_ptr(), self.argv.as_ptr(), self.envp.as_ptr()) };
    }
}

/// Mark the child's actual post-fork descriptor table. A parent-side snapshot
/// could miss an inheritable fd opened concurrently by another transport thread.
fn cloexec_descriptors() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: close_range with CLOEXEC only updates this forked child's
        // descriptor flags. It takes integers, retains no memory and reports
        // errno; libc::syscall performs no allocation or user-space locking.
        if unsafe {
            libc::syscall(
                libc::SYS_close_range,
                3_u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        } == 0
        {
            return Ok(());
        }
    }
    let directory = if cfg!(target_os = "linux") {
        c"/proc/self/fd"
    } else {
        c"/dev/fd"
    };
    // SAFETY: static NUL-terminated path and valid no-create open flags. open
    // is async-signal-safe and returns an owned fd or thread-local errno.
    let fd = unsafe {
        libc::open(
            directory.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = mark_directory_fds(fd);
    // SAFETY: this function alone owns the directory fd, with no surviving
    // borrowers. close is async-signal-safe and its failure is returned.
    let closed = unsafe { libc::close(fd) };
    result?;
    if closed < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn mark_directory_fds(directory: i32) -> io::Result<()> {
    let mut buffer = [0_u8; 8192];
    loop {
        #[cfg(target_os = "linux")]
        // SAFETY: getdents64 writes at most buffer.len() bytes to the live
        // exclusive buffer and advances our owned directory fd. Raw syscall
        // needs no allocator or lock in the forked child; errors use errno.
        let count = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                directory,
                buffer.as_mut_ptr(),
                buffer.len(),
            )
        };
        #[cfg(target_os = "macos")]
        let count = {
            let mut base = 0_i64;
            // SAFETY: Darwin SYS_getdirentries64=344 (SDK sys/syscall.h).
            // The syscall writes at most buffer.len() bytes plus the live
            // off_t base, retaining no pointers. Calling the raw syscall
            // avoids readdir's allocation/locks after fork.
            #[allow(deprecated)]
            unsafe {
                libc::syscall(344, directory, buffer.as_mut_ptr(), buffer.len(), &mut base)
            }
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Ok(());
        }
        let bytes = buffer
            .get(..count as usize)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
        let mut offset = 0;
        while offset < bytes.len() {
            // Both dirent64 layouts have d_reclen at offset 16. Darwin's
            // d_name follows d_namlen/d_type at 21; Linux's follows d_type at 19.
            let name_offset = if cfg!(target_os = "macos") { 21 } else { 19 };
            let record = &bytes[offset..];
            let length = record
                .get(16..18)
                .and_then(|b| b.try_into().ok())
                .map(u16::from_ne_bytes)
                .map(usize::from)
                .filter(|&len| len > name_offset && len <= record.len())
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
            let name = &record[name_offset..length];
            let end = name
                .iter()
                .position(|&byte| byte == 0)
                .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?;
            if let Some(fd) = descriptor_number(&name[..end]).filter(|&fd| fd > 2) {
                // SAFETY: fcntl acts only on the forked child's integral fd;
                // setting CLOEXEC retains ownership and cannot close std's
                // exec-error pipe before it reports a failed pre_exec hook.
                unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags >= 0 {
                        if libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
                            return Err(io::Error::last_os_error());
                        }
                    } else if io::Error::last_os_error().raw_os_error() != Some(libc::EBADF) {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            offset += length;
        }
    }
}

fn descriptor_number(name: &[u8]) -> Option<i32> {
    if name.is_empty() {
        return None;
    }
    name.iter().try_fold(0_i32, |number, &byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        number.checked_mul(10)?.checked_add(i32::from(byte - b'0'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{FromRawFd, OwnedFd};

    #[test]
    fn utility_exec_closes_an_inheritable_descriptor_not_explicitly_passed() {
        let file = tempfile::tempfile().unwrap();
        // SAFETY: fcntl duplicates a borrowed live fd into a new owned fd;
        // F_DUPFD deliberately clears CLOEXEC, exercising the inheritance rule.
        // No other thread owns the duplicate; negative results are checked.
        let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 200) };
        assert!(fd >= 200, "{}", io::Error::last_os_error());
        // SAFETY: the successful duplication above transfers this unique fd
        // to OwnedFd; it is closed once after the fixture has been reaped.
        let inherited = unsafe { OwnedFd::from_raw_fd(fd) };
        let output = crate::bounded::run_bounded(
            &[
                "/bin/sh".into(),
                "-c".into(),
                format!("if test -e /dev/fd/{fd}; then printf LEAK; else printf CLOSED; fi").into(),
            ],
            crate::bounded::Options::new(std::path::Path::new("/"), 5.0),
        )
        .unwrap();
        assert_eq!(output.output, "CLOSED");
        drop(inherited);
    }
}

#[cfg(test)]
mod exit_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_running_child_returns_a_typed_reap_timeout() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "read line"])
            .stdin(std::process::Stdio::piped());
        let mut child = OwnedChild::spawn(&mut command, &[], None).unwrap();
        assert!(matches!(
            child.reap(Duration::ZERO),
            Err(ChildWaitError::Timeout {
                timeout: Duration::ZERO,
                ..
            })
        ));
        child.kill_direct().unwrap();
        child.reap(Duration::from_millis(250)).unwrap();
        child.finish_cleanup();
    }

    #[test]
    fn observed_trivial_exit_does_not_keep_its_group_alive() {
        let mut command = Command::new("/usr/bin/true");
        let mut child = OwnedChild::spawn(&mut command, &[], None).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !child.exited().unwrap() {
            assert!(Instant::now() < deadline, "fixture did not exit");
            std::thread::yield_now();
        }
        assert!(
            !child.group_exists(),
            "the zombie leader must be reaped before probing"
        );
        assert_eq!(
            child.reap(Duration::from_millis(250)).unwrap().code(),
            Some(0)
        );
        child.finish_cleanup();
    }
}
