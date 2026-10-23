//! Narrow safe POSIX wrappers. No provider signalling API is exposed.
//!
//! Linux and macOS share these libc interfaces. Account lookup uses the
//! reentrant calls so the wrapper is safe even after transport threads start.

use std::collections::BTreeMap;
use std::convert::Infallible;
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

/// Strict execve: ENOEXEC is returned without Command::exec's shell fallback.
/// The caller supplies argv[0], the full environment, and edge error rendering.
pub fn execve_strict(
    program: &Path,
    argv: &[OsString],
    env: &BTreeMap<OsString, OsString>,
) -> Result<Infallible, SysError> {
    let failure = |source| SysError {
        operation: "execve",
        path: Some(program.into()),
        source,
    };
    let c_string = |bytes: &[u8]| {
        CString::new(bytes).map_err(|_| failure(io::Error::from_raw_os_error(libc::EINVAL)))
    };
    let path = c_string(program.as_os_str().as_bytes())?;
    let argv = argv
        .iter()
        .map(|arg| c_string(arg.as_bytes()))
        .collect::<Result<Vec<_>, _>>()?;
    let envp = env
        .iter()
        .map(|(name, value)| {
            let mut bytes = name.as_bytes().to_vec();
            bytes.push(b'=');
            bytes.extend_from_slice(value.as_bytes());
            c_string(&bytes)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let argv_ptrs = argv
        .iter()
        .map(|arg| arg.as_ptr())
        .chain([std::ptr::null()])
        .collect::<Vec<_>>();
    let env_ptrs = envp
        .iter()
        .map(|entry| entry.as_ptr())
        .chain([std::ptr::null()])
        .collect::<Vec<_>>();
    // SAFETY: all strings and the two null-terminated pointer arrays remain
    // owned and live until execve returns or replaces this process. The call
    // takes no Rust ownership; it either atomically replaces all threads or
    // returns -1 with thread-local errno. No environment globals are mutated.
    unsafe { libc::execve(path.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr()) };
    Err(failure(io::Error::last_os_error()))
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
        let error = execve_strict(&file, &[file.as_os_str().into()], &BTreeMap::new()).unwrap_err();
        assert_eq!(error.source.raw_os_error(), Some(libc::ENOEXEC));
    }

    #[test]
    fn exec_rejects_nuls_in_every_input() {
        let bad = OsString::from_vec(b"bad\0arg".to_vec());
        let file = Path::new("/no/such/file");
        for (argv, env) in [
            (vec![bad.clone()], BTreeMap::new()),
            (vec![], BTreeMap::from([(bad.clone(), "value".into())])),
            (vec![], BTreeMap::from([("key".into(), bad)])),
        ] {
            let error = execve_strict(file, &argv, &env).unwrap_err();
            assert_eq!(error.source.raw_os_error(), Some(libc::EINVAL));
        }
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

    #[test]
    fn exec_fixture() {
        if std::env::var_os("PROVENANT_SUPERVISOR_EXEC_FIXTURE").is_none() {
            return;
        }
        let env = BTreeMap::from([("KEPT".into(), "λ two words".into())]);
        let args = ["/bin/sh".into(), "-c".into(), "test \"$KEPT\" = 'λ two words' && test -z \"$PROVENANT_SUPERVISOR_EXEC_FIXTURE\" && exit 37".into()];
        let error = execve_strict(Path::new("/bin/sh"), &args, &env).unwrap_err();
        panic!("exec failed: {error}");
    }

    #[test]
    fn successful_exec_replaces_the_process_with_the_exact_environment() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "sys::tests::exec_fixture", "--nocapture"])
            .env("PROVENANT_SUPERVISOR_EXEC_FIXTURE", "1")
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(37));
    }
}

mod child;
pub(crate) mod pid;
pub(crate) mod raw_exec;
pub use child::ChildWaitError;
pub(crate) use child::{OwnedChild, anonymous_spool};

pub mod lane_files;
pub mod process;

pub(crate) mod fs;

/// Named platform errno values; consumers must not import libc.
pub mod errno {
    pub use libc::{
        EACCES, EEXIST, EINTR, EINVAL, EISDIR, ELOOP, ENOENT, ENOEXEC, ENOSYS, ENOTDIR, ENOTSUP,
        EOPNOTSUPP, EPERM, EXDEV,
    };
}

// Existing S2 implementation, pending the supervisor rewrite.
#[doc(hidden)]
pub mod legacy_lane;
