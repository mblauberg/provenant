//! Narrow safe POSIX wrappers. No provider signalling API is exposed.
//!
//! Linux and macOS share these libc interfaces. Account lookup uses the
//! reentrant calls so the wrapper is safe even after transport threads start.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{operation} failed")]
pub struct SysError {
    pub operation: &'static str,
    pub path: Option<PathBuf>,
    #[source]
    pub source: io::Error,
}

pub fn is_executable(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: path owns a NUL-terminated buffer for the duration of the call;
    // access borrows it, mutates no Rust memory, and returns -1 on failure.
    // No shared libc storage is exposed and concurrent calls are independent.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

pub enum Account<'a> {
    Current,
    Named(&'a OsStr),
    Uid(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    pub name: OsString,
    pub home: OsString,
}

pub fn account(account: Account<'_>) -> Result<Option<AccountInfo>, SysError> {
    let failure = |source| SysError {
        operation: "passwd lookup",
        path: None,
        source,
    };
    let name = match account {
        Account::Named(name) => Some(
            CString::new(name.as_bytes())
                .map_err(|_| failure(io::Error::from_raw_os_error(libc::EINVAL)))?,
        ),
        _ => None,
    };
    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let status = match account {
            Account::Current => {
                // SAFETY: getuid reads only the current real UID and retains
                // no pointers or mutable process state.
                let uid = unsafe { libc::getuid() };
                // SAFETY: entry, result and buffer are disjoint live
                // allocations of the required sizes. The _r call borrows the
                // buffers and places returned strings inside our buffer.
                unsafe {
                    libc::getpwuid_r(
                        uid,
                        entry.as_mut_ptr(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        &mut result,
                    )
                }
            }
            Account::Uid(uid) => {
                // SAFETY: entry, result and buffer are disjoint live
                // allocations of the required sizes. The _r call borrows the
                // buffers and places returned strings inside our buffer.
                unsafe {
                    libc::getpwuid_r(
                        uid,
                        entry.as_mut_ptr(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        &mut result,
                    )
                }
            }
            Account::Named(_) => match &name {
                Some(name) => {
                    // SAFETY: named input is NUL-terminated; entry, result
                    // and buffer are disjoint live allocations of the
                    // required sizes. Returned strings point into buffer.
                    unsafe {
                        libc::getpwnam_r(
                            name.as_ptr(),
                            entry.as_mut_ptr(),
                            buffer.as_mut_ptr().cast(),
                            buffer.len(),
                            &mut result,
                        )
                    }
                }
                None => return Err(failure(io::Error::from_raw_os_error(libc::EINVAL))),
            },
        };
        if status == libc::EINTR {
            continue;
        }
        if status == libc::ERANGE {
            let Some(size) = buffer.len().checked_mul(2) else {
                return Err(failure(io::Error::from_raw_os_error(libc::ERANGE)));
            };
            buffer.resize(size, 0);
            continue;
        }
        if status != 0 {
            return Err(failure(io::Error::from_raw_os_error(status)));
        }
        if result.is_null() {
            return Ok(None);
        }
        // SAFETY: success with non-null result means getpw*_r initialised entry.
        let entry = unsafe { entry.assume_init() };
        if entry.pw_dir.is_null() {
            return Ok(None);
        }
        let name = if entry.pw_name.is_null() {
            OsString::new()
        } else {
            // SAFETY: the successful _r call returned a NUL-terminated name
            // pointer into our live buffer; it is copied before the next call.
            OsString::from_vec(unsafe { CStr::from_ptr(entry.pw_name) }.to_bytes().to_vec())
        };
        // SAFETY: the successful _r call returned a NUL-terminated home
        // pointer into our live buffer; it is copied before the next call.
        let home = OsString::from_vec(unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes().to_vec());
        return Ok(Some(AccountInfo { name, home }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn access_follows_links_and_checks_real_id_execution() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tool");
        std::fs::write(&file, "echo no shebang\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(&file));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&file, dir.path().join("link")).unwrap();
        assert!(is_executable(&dir.path().join("link")));
        assert!(!is_executable(&dir.path().join("gone")));
        assert!(!is_executable(Path::new(OsStr::from_bytes(b"bad\0path"))));
    }

    #[test]
    fn passwd_lookup_copies_home_and_handles_unknown_name() {
        let current = account(Account::Current).unwrap().unwrap();
        assert!(Path::new(&current.home).is_absolute());
        assert_eq!(
            account(Account::Named(&current.name)).unwrap().unwrap(),
            current
        );
        assert_eq!(
            account(Account::Named(OsStr::new("provenant_missing_user_zz"))).unwrap(),
            None
        );
    }
}

#[doc(hidden)]
pub mod child;
pub(crate) mod pid;
#[doc(hidden)]
pub mod raw_exec;

#[doc(hidden)]
pub mod fs;

/// Named platform errno values; consumers must not import libc.
pub mod errno {
    pub use libc::{
        EACCES, EEXIST, EINTR, EINVAL, EISDIR, ELOOP, ENOENT, ENOEXEC, ENOSPC, ENOSYS, ENOTDIR,
        ENOTSUP, EOPNOTSUPP, EPERM, EXDEV,
    };
}

#[doc(hidden)]
pub mod reaper;
pub mod resources;

/// Construct the fixture binary's only child command through the same strict
/// post-fork boundary used by the supervisor.
#[cfg(feature = "fixture")]
#[allow(clippy::disallowed_methods)]
pub fn fixture_command(
    argv: &[OsString],
    env: &[(OsString, OsString)],
    cwd: &Path,
    new_session: bool,
) -> io::Result<std::process::Command> {
    let program = argv
        .first()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    if std::path::Path::new(program) != std::env::current_exe()? {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut command = std::process::Command::new(program);
    command
        .args(argv.iter().skip(1))
        .env_clear()
        .envs(env.iter().cloned())
        .current_dir(cwd);
    child::configure_command(&mut command, &[], Some(env), new_session)?;
    Ok(command)
}

/// Accept a fixture connection with one bounded poll of a held Unix listener.
#[cfg(feature = "fixture")]
pub fn fixture_accept(
    listener: &std::os::unix::net::UnixListener,
    timeout: std::time::Duration,
) -> io::Result<std::os::unix::net::UnixStream> {
    use std::os::fd::AsRawFd;
    let deadline = std::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let micros = remaining.as_micros();
        let millis = if remaining.is_zero() {
            0
        } else {
            i32::try_from(micros.saturating_add(999) / 1_000).unwrap_or(i32::MAX)
        };
        let mut descriptor = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor is a live borrowed listener fd and descriptor's
        // one-element storage stays writable through poll. The timeout is
        // derived from one checked deadline; poll retains no pointers/ownership.
        let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if result > 0 {
            if descriptor.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            return listener.accept().map(|(stream, _)| stream);
        }
        if result == 0 {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
    }
}

/// Read one fixture handshake byte with a bounded readiness wait.
#[cfg(feature = "fixture")]
pub fn fixture_read_byte(
    stream: &mut std::os::unix::net::UnixStream,
    timeout: std::time::Duration,
) -> io::Result<u8> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    let deadline = std::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let micros = remaining.as_micros();
        let millis = if remaining.is_zero() {
            0
        } else {
            i32::try_from(micros.saturating_add(999) / 1_000).unwrap_or(i32::MAX)
        };
        let mut descriptor = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor borrows a live stream fd and its one-element
        // storage remains writable through poll. The timeout is bounded by a
        // checked deadline; poll retains neither memory nor descriptor.
        let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if result > 0 {
            if descriptor.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            let mut byte = [0];
            return stream.read_exact(&mut byte).map(|()| byte[0]);
        }
        if result == 0 {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
    }
}

/// Replace the current process with a selected owner. Argument parsing remains
/// at the binary edge; no shell fallback or Python delegation is introduced.
pub fn replace_process(
    program: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
) -> io::Error {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)));
    child::replace_process(&command, env)
}

/// Read a fixture's inherited secret directly, without opening `/dev/fd/N`
/// through a filesystem policy. Duplicate it so the caller keeps its reader.
#[cfg(feature = "fixture")]
pub fn fixture_read_secret_pipe(fd: i32) -> io::Result<Vec<u8>> {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    if fd <= 2 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: fcntl validates the numeric descriptor and returns a fresh owned
    // duplicate or an error. It retains no pointers and leaves fd's flags alone.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful F_DUPFD_CLOEXEC returns a new owning descriptor;
    // this File is its sole owner and closes it on every return path.
    let file = unsafe { std::fs::File::from_raw_fd(copy) };
    let mut bytes = Vec::new();
    file.take(513).read_to_end(&mut bytes)?;
    if bytes.len() > 512 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

/// Create deliberately inheritable state only in the isolated fixture process.
#[cfg(feature = "fixture")]
pub fn fixture_parent_state() -> io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let file = std::fs::File::open("/dev/null")?;
    // SAFETY: file is live; F_DUPFD returns a distinct owned descriptor without
    // CLOEXEC. A high minimum avoids incidental descriptor reuse in the probe.
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 200) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_DUPFD succeeded and this is the sole owner of its new fd.
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    fixture_ignore_term()?;
    // SAFETY: all bit patterns are valid for sigset_t; sigemptyset initialises
    // it before use and both calls borrow exclusive live stack storage.
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: mask is valid writable storage, no pointer is retained.
    if unsafe { libc::sigemptyset(&mut mask) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: SIGUSR2 is valid and mask is initialised and exclusively borrowed.
    if unsafe { libc::sigaddset(&mut mask, libc::SIGUSR2) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: only this dedicated fixture thread changes its own signal mask.
    if unsafe { libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

#[cfg(feature = "fixture")]
pub fn fixture_check_inherited_state(fd: i32, parent: bool) -> io::Result<()> {
    // SAFETY: F_GETFD inspects an integer fd without taking ownership.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    let descriptor_ok = if parent {
        flags >= 0 && flags & libc::FD_CLOEXEC == 0
    } else {
        flags < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
    };
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    // SAFETY: this only queries SIGTERM into valid exclusive storage.
    if unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), action.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful sigaction initialised the record.
    let action = unsafe { action.assume_init() };
    let mut mask = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: null set queries the calling thread's mask into valid storage.
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, std::ptr::null(), mask.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful query initialised mask.
    let mask = unsafe { mask.assume_init() };
    // SAFETY: initialised mask and a valid signal are borrowed, not retained.
    let blocked = unsafe { libc::sigismember(&mask, libc::SIGUSR2) };
    let expected = if parent { libc::SIG_IGN } else { libc::SIG_DFL };
    if descriptor_ok && action.sa_sigaction == expected && blocked == i32::from(parent) {
        Ok(())
    } else {
        Err(io::Error::other(
            "descriptor flags, signal disposition or mask changed incorrectly",
        ))
    }
}

/// Enter a sandbox that denies every Mach lookup except the attempt's own
/// name, the shape of the production profile for this purpose. Irreversible.
#[cfg(all(feature = "fixture", target_os = "macos"))]
pub fn fixture_enter_attempt_sandbox(lookup_name: &str) -> io::Result<()> {
    unsafe extern "C" {
        fn sandbox_init(
            profile: *const libc::c_char,
            flags: u64,
            errorbuf: *mut *mut libc::c_char,
        ) -> libc::c_int;
    }
    let profile = std::ffi::CString::new(format!(
        "(version 1)(allow default)(deny mach-lookup)(allow mach-lookup (global-name \"{lookup_name}\"))"
    ))
    .map_err(io::Error::other)?;
    // SAFETY: profile is a live NUL-terminated string; flags 0 means the
    // argument is SBPL source; a null error buffer is accepted and the call
    // retains no pointer. It confines only this dedicated fixture process.
    if unsafe { sandbox_init(profile.as_ptr(), 0, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::other("fixture sandbox refused"));
    }
    Ok(())
}

#[cfg(feature = "fixture")]
pub fn fixture_ignore_term() -> io::Result<()> {
    // SAFETY: the dedicated fixture changes only its own disposition; SIG_IGN
    // is a valid platform handler constant, no resource or pointer transfers,
    // and failure reports thread-local errno. Never used in a shared host.
    if unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) } == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Used only in a dedicated, single-threaded fixture process before any spawn.
#[cfg(feature = "fixture")]
pub fn fixture_wait_policy(mode: &str) -> io::Result<()> {
    extern "C" fn handler(_: libc::c_int) {}
    // SAFETY: libc's sigaction record permits zeroed integers, pointer-sized
    // handler and mask storage. sigemptyset below initialises its platform mask.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: action owns writable, aligned mask storage; no pointer is retained.
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
        return Err(io::Error::last_os_error());
    }
    match mode {
        "wait-policy-ignore" => action.sa_sigaction = libc::SIG_IGN,
        "wait-policy-no-wait" => action.sa_flags = libc::SA_NOCLDWAIT,
        "wait-policy-handler" => action.sa_sigaction = handler as *const () as usize,
        "wait-policy-blocked" => {
            // SAFETY: the fixture owns the live mask storage; sigaddset
            // mutates only that object and retains no pointer.
            if unsafe { libc::sigaddset(&mut action.sa_mask, libc::SIGCHLD) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: this dedicated fixture changes only its own thread mask;
            // the pointer is borrowed for the call and is not retained.
            if unsafe { libc::sigprocmask(libc::SIG_BLOCK, &action.sa_mask, std::ptr::null_mut()) }
                != 0
            {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    }
    // SAFETY: the dedicated fixture sets its own process disposition before
    // starting threads or children. Handler constants/function are valid; libc
    // copies the live record and retains no pointer. No shared host is changed.
    if unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One bounded scheduler pause. Callers recheck cancellation between pauses.
pub fn wait_pause(duration: std::time::Duration) {
    std::thread::sleep(duration.min(std::time::Duration::from_millis(50)));
}

/// All production clocks use this process-wide monotonic coordinate.
#[allow(clippy::disallowed_methods)]
pub fn monotonic_now() -> provenant_core::MonoTime {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let elapsed = ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_micros();
    provenant_core::MonoTime::from_micros(u64::try_from(elapsed).unwrap_or(u64::MAX))
}

pub mod hook_io;
