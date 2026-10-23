//! Strict exec boundary shared by direct utility runners.
use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::process::Command;

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
            reset_signals()?;
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

/// Reset inherited dispositions and the calling child's signal mask before exec.
fn reset_signals() -> io::Result<()> {
    // SAFETY: invoked only after fork. Stack signal records are live, exclusive,
    // fully initialised before use; sigaction/procmask retain no pointer. Only
    // async-signal-safe calls run, with no parent/global mutation or allocation.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        if libc::sigemptyset(&mut action.sa_mask) < 0 {
            return Err(io::Error::last_os_error());
        }
        #[cfg(target_os = "linux")]
        let maximum = 64;
        #[cfg(target_os = "macos")]
        let maximum = 31;
        for signal in 1..=maximum {
            if signal == libc::SIGKILL || signal == libc::SIGSTOP {
                continue;
            }
            if libc::sigaction(signal, &action, std::ptr::null_mut()) < 0 {
                let error = io::Error::last_os_error();
                // Linux reserves libc's internal realtime signals 32 and 33.
                if error.raw_os_error() != Some(libc::EINVAL) {
                    return Err(error);
                }
            }
        }
        if libc::sigprocmask(libc::SIG_SETMASK, &action.sa_mask, std::ptr::null_mut()) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
