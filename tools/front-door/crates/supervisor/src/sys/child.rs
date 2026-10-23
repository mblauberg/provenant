//! Child-only custody for the bounded utility runner, separate from S2 providers.
use crate::identity::Signal;
use std::collections::BTreeMap;
use std::ffi::{CString, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};

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

/// The direct child stays waitable (WNOWAIT) until group cleanup has finished.
/// Its PID cannot be reused while this owner holds its unreaped exit identity.
pub(crate) struct OwnedChild {
    child: Child,
    reaped: bool,
}

impl OwnedChild {
    pub(crate) fn spawn(
        command: &mut Command,
        pass_fds: &[BorrowedFd<'_>],
        env: Option<&BTreeMap<OsString, OsString>>,
    ) -> io::Result<Self> {
        let pass: Vec<_> = pass_fds.iter().map(AsRawFd::as_raw_fd).collect();
        let directory = if cfg!(target_os = "linux") {
            "/proc/self/fd"
        } else {
            "/dev/fd"
        };
        let descriptors = std::fs::read_dir(directory)?
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .filter(|&fd| fd > 2)
            .collect::<Vec<_>>();
        let env: BTreeMap<_, _> = env
            .cloned()
            .unwrap_or_else(|| std::env::vars_os().collect());
        let string = |bytes: &[u8]| {
            CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
        };
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
                .get(std::ffi::OsStr::new("PATH"))
                .map(|value| value.as_bytes())
                .unwrap_or(b"/bin:/usr/bin");
            path.split(|&byte| byte == b':')
                .map(|dir| {
                    string(&[dir, if dir.is_empty() { b"" } else { b"/" }, program].concat())
                })
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
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                for &fd in &descriptors {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 {
                        if io::Error::last_os_error().raw_os_error() == Some(libc::EBADF) {
                            continue;
                        }
                        return Err(io::Error::last_os_error());
                    }
                    if libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
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
        Ok(Self {
            child: command.spawn()?,
            reaped: false,
        })
    }

    pub(crate) fn exited(&self) -> io::Result<bool> {
        // SAFETY: siginfo is zero-initialised for the WNOHANG/no-event case.
        // waitid writes at most its size, borrows no other memory, and this
        // owner alone waits for the positive child PID. WNOWAIT retains custody.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            if libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(info.si_pid() != 0)
        }
    }

    pub(crate) fn group_exists(&self) -> bool {
        // SAFETY: the unreaped child reserved this group ID with setsid before
        // spawn returned. Signal 0 has no effect; only ESRCH proves absence.
        let status = unsafe { libc::kill(-(self.child.id() as i32), 0) };
        status == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    pub(crate) fn signal(&self, signal: Signal) -> io::Result<()> {
        let sig = match signal {
            Signal::Terminate => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: the child's unreaped PID reserves its session/group ID. This
        // private handle cannot target an arbitrary provider or sibling group.
        // kill takes no ownership and reports errors through thread-local errno.
        if unsafe { libc::kill(-(self.child.id() as i32), sig) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(error)
    }

    pub(crate) fn kill_direct(&mut self) -> io::Result<()> {
        self.child.kill()
    }

    pub(crate) fn reap(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Best effort on unwinding only. The normal path reports errors and
        // explicitly reaps last; Drop makes no guarantee on abort or SIGKILL.
        if !self.reaped {
            let _ = self.signal(Signal::Kill);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
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
