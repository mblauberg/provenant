//! Process replacement and its environment rules. Every verb ends here, so
//! the PID, stdio, working directory and signals all reach the owner untouched.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
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

/// `os.access(path, X_OK)`: real uid and gid, symlinks followed.
pub use provenant_supervisor::sys::is_executable;

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
    let mut argv = vec![program];
    argv.extend_from_slice(launch.args);
    let error = match provenant_supervisor::sys::execve_strict(launch.program, &argv, &env) {
        Err(error) => error.source,
        Ok(never) => match never {},
    };
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
