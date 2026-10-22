//! Process replacement and its environment rules. Every verb ends here, so
//! the PID, stdio, working directory and signals all reach the owner untouched.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, CString, OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub const REPAIR: &str =
    "repair: re-run install-harness from the product checkout, or set AGENT_FABRIC_PRODUCT_ROOT";

/// Git variables that would redirect a checker away from the checkout it
/// was selected for. Only `check` removes them.
pub const GIT_REDIRECTS: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

unsafe extern "C" {
    fn access(path: *const c_char, mode: c_int) -> c_int;
    fn execve(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char)
        -> c_int;
}

const X_OK: c_int = 1;

/// `os.access(path, X_OK)`: real uid and gid, symlinks followed.
pub fn is_executable(path: &Path) -> bool {
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    if bytes.contains(&0) {
        return false;
    }
    bytes.push(0);
    // SAFETY: `bytes` is a NUL-terminated buffer that outlives the call.
    unsafe { access(bytes.as_ptr().cast(), X_OK) == 0 }
}

/// What to run and how to adjust its environment.
pub struct Launch<'a> {
    pub program: &'a Path,
    pub args: &'a [OsString],
    /// When set, `AGENT_FABRIC_PRODUCT_ROOT` and `AGENTS_HOME` are pointed at it.
    pub root: Option<&'a Path>,
    pub scrub_git: bool,
    pub stale_checked: bool,
}

/// The libc error text without Rust's ` (os error N)` suffix.
pub fn describe(error: &io::Error) -> String {
    let text = error.to_string();
    match text.rfind(" (os error") {
        Some(end) => text[..end].to_string(),
        None => text,
    }
}

/// `execve(2)` itself. `Command::exec` retries a shebangless file through
/// `/bin/sh` on ENOEXEC (the `execvp` fallback); Python's `os.execve` does not,
/// so a file the kernel cannot run is an error here too.
fn execve_strict(
    program: &Path,
    argv: &[&OsString],
    env: &BTreeMap<OsString, OsString>,
) -> io::Error {
    fn c_string(bytes: &[u8]) -> Result<CString, io::Error> {
        CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(22))
    }
    let build = || -> Result<_, io::Error> {
        let path = c_string(program.as_os_str().as_bytes())?;
        let argv = argv
            .iter()
            .map(|arg| c_string(arg.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        let envp = env
            .iter()
            .map(|(name, value)| {
                let mut entry = name.as_bytes().to_vec();
                entry.push(b'=');
                entry.extend_from_slice(value.as_bytes());
                c_string(&entry)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((path, argv, envp))
    };
    let (path, argv, envp) = match build() {
        Ok(parts) => parts,
        Err(error) => return error,
    };
    let argv_ptrs: Vec<*const c_char> = argv
        .iter()
        .map(|a| a.as_ptr())
        .chain([std::ptr::null()])
        .collect();
    let env_ptrs: Vec<*const c_char> = envp
        .iter()
        .map(|a| a.as_ptr())
        .chain([std::ptr::null()])
        .collect();
    // SAFETY: every pointer is into a live NUL-terminated CString and both
    // arrays end with a null pointer.
    unsafe { execve(path.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr()) };
    io::Error::last_os_error()
}

/// Replace this process. Returns only on failure, with the exit status to use.
pub fn replace_process(launch: &Launch) -> u8 {
    let mut env: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    if let Some(root) = launch.root {
        // The instance root is never defaulted here: inventing one would turn
        // a default into an explicit selection (#563).
        env.insert("AGENT_FABRIC_PRODUCT_ROOT".into(), root.into());
        env.insert("AGENTS_HOME".into(), root.into());
    }
    if launch.scrub_git {
        for name in GIT_REDIRECTS {
            env.remove(OsStr::new(name));
        }
    }
    if launch.stale_checked {
        env.insert("PROVENANT_STALE_CHECKED".into(), "1".into());
    }
    let program: OsString = launch.program.as_os_str().to_owned();
    let mut argv: Vec<&OsString> = vec![&program];
    argv.extend(launch.args.iter());
    let error = execve_strict(launch.program, &argv, &env);
    eprintln!(
        "provenant: cannot execute {}: {}; {REPAIR}",
        launch.program.display(),
        describe(&error)
    );
    3
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn describe_drops_the_os_error_suffix() {
        let error = io::Error::from_raw_os_error(2);
        assert_eq!(describe(&error), "No such file or directory");
    }

    #[test]
    fn executable_bit_is_checked_against_real_ids() {
        let dir = std::env::temp_dir().join(format!("fd-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tool");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(&file));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&file));
        assert!(!is_executable(&dir.join("missing")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn git_scrub_list_is_the_eight_redirects() {
        assert_eq!(GIT_REDIRECTS.len(), 8);
        assert!(GIT_REDIRECTS.iter().all(|name| name.starts_with("GIT_")));
    }
}
