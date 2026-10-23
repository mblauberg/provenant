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
        // SAFETY: entry, result and the writable buffer are disjoint live
        // allocations of the required sizes. Named input is NUL-terminated.
        // The _r calls borrow the buffers, return an errno (not errno global),
        // and put any returned string pointers inside buffer. No static passwd
        // storage or owning handles escape; concurrent calls are independent.
        let status = unsafe {
            match account {
                Account::Current => libc::getpwuid_r(
                    libc::getuid(),
                    entry.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                ),
                Account::Uid(uid) => libc::getpwuid_r(
                    uid,
                    entry.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                ),
                Account::Named(_) => match &name {
                    Some(name) => libc::getpwnam_r(
                        name.as_ptr(),
                        entry.as_mut_ptr(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        &mut result,
                    ),
                    None => return Err(failure(io::Error::from_raw_os_error(libc::EINVAL))),
                },
            }
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
        // SAFETY: success with non-null result initialises entry. Both string
        // pointers refer to NUL-terminated strings in our live buffer; copy
        // them before the next call or buffer drop. Null fields are refused.
        // Nothing is shared with other threads or transferred to libc.
        unsafe {
            let entry = entry.assume_init();
            if entry.pw_dir.is_null() {
                return Ok(None);
            }
            return Ok(Some(AccountInfo {
                name: if entry.pw_name.is_null() {
                    OsString::new()
                } else {
                    OsString::from_vec(CStr::from_ptr(entry.pw_name).to_bytes().to_vec())
                },
                home: OsString::from_vec(CStr::from_ptr(entry.pw_dir).to_bytes().to_vec()),
            }));
        }
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

mod child;
pub(crate) mod pid;
pub(crate) mod raw_exec;

pub mod lane_files;

pub(crate) mod fs;

/// Named platform errno values; consumers must not import libc.
pub mod errno {
    pub use libc::{
        EACCES, EEXIST, EINTR, EINVAL, EISDIR, ELOOP, ENOENT, ENOEXEC, ENOSYS, ENOTDIR, ENOTSUP,
        EOPNOTSUPP, EPERM, EXDEV,
    };
}

pub(crate) mod reaper;

/// Replace the current process with a selected owner. Argument parsing remains
/// at the binary edge; no shell fallback or Python delegation is introduced.
pub fn replace_process(
    program: &Path,
    args: &[OsString],
    env: &[(OsString, OsString)],
) -> io::Error {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)));
    if let Err(error) = child::configure_command(&mut command, &[], Some(env), false) {
        return error;
    }
    command.exec()
}

#[cfg(feature = "fixture")]
pub(crate) fn fixture_ignore_term() -> io::Result<()> {
    // SAFETY: the dedicated fixture changes only its own disposition; SIG_IGN
    // is a valid platform handler constant, no resource or pointer transfers,
    // and failure reports thread-local errno. Never used in a shared host.
    if unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) } == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// Existing lane subjects remain until S2 replaces custody. These narrow
// allowances cover inherited diagnostics and pre-R7 unsafe grouping only.
#[allow(clippy::print_stderr, clippy::collapsible_if, clippy::unwrap_used,
        clippy::undocumented_unsafe_blocks, clippy::multiple_unsafe_ops_per_block)]
#[doc(hidden)]
pub mod legacy_lane;
