//! Host Git executable selection shared by every product owner.
use crate::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use std::{
    ffi::{OsStr, OsString},
    fs, io,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::Duration,
};

/// Git path to discover from a checkout using an isolated environment.
#[derive(Clone, Copy)]
pub enum RepositoryPath {
    Toplevel,
    CommonDir,
}

/// Resolve and validate a repository path without inheriting Git redirects.
pub fn repository_path(cwd: &Path, field: RepositoryPath) -> io::Result<PathBuf> {
    let git = git_executable().ok_or_else(|| io::Error::other("Git executable unavailable"))?;
    let flag = match field {
        RepositoryPath::Toplevel => "--show-toplevel",
        RepositoryPath::CommonDir => "--git-common-dir",
    };
    let argv: Vec<OsString> = [
        git.as_os_str(),
        OsStr::new("--no-pager"),
        OsStr::new("--no-optional-locks"),
        OsStr::new("-c"),
        OsStr::new("core.fsmonitor=false"),
        OsStr::new("-c"),
        OsStr::new("core.hooksPath=/dev/null"),
        OsStr::new("rev-parse"),
        OsStr::new("--path-format=absolute"),
        OsStr::new(flag),
    ]
    .into_iter()
    .map(OsStr::to_owned)
    .collect();
    let env: Vec<(OsString, OsString)> = [
        ("HOME", "/nonexistent"),
        ("XDG_CONFIG_HOME", "/nonexistent"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .map(|(k, v)| (k.into(), v.into()))
    .into();
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(2));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Discard;
    options.stop_on_output_limit = true;
    let result = SystemExecutor::new(1)
        .and_then(|executor| executor.execute(options))
        .map_err(io::Error::other)?;
    if result.termination != Termination::Exit(0)
        || result.timed_out
        || result.drain_truncated
        || result.stdout.truncated
        || result.stdout.read_failed
    {
        return Err(io::Error::other("Git path discovery failed"));
    }
    parse_repository_path(&result.stdout.bytes)
}

fn parse_repository_path(output: &[u8]) -> io::Result<PathBuf> {
    let bytes = output
        .strip_suffix(b"\n")
        .ok_or_else(|| io::Error::other("incomplete Git path"))?;
    if bytes.is_empty() || bytes.contains(&0) || bytes.contains(&b'\n') {
        return Err(io::Error::other("invalid Git path"));
    }
    let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    if !path.is_absolute() {
        return Err(io::Error::other("relative Git path"));
    }
    fs::canonicalize(path)
}

const CANDIDATES: [&str; 6] = [
    "/opt/homebrew/bin/git",
    "/usr/local/bin/git",
    "/opt/local/bin/git",
    // The real CLT executable avoids /usr/bin/git's xcrun loader, which
    // otherwise needs access to the active Xcode installation in a lane.
    "/Library/Developer/CommandLineTools/usr/bin/git",
    "/usr/bin/git",
    "/bin/git",
];

/// Resolve the override or the first executable in the fixed trusted list.
/// PATH is deliberately not searched: Apple's `/usr/bin/git` lacks
/// `--attr-source`, so a newer package-manager Git must win when both exist.
pub fn resolve_git_executable(override_path: Option<&Path>) -> Option<PathBuf> {
    resolve(override_path, &CANDIDATES.map(PathBuf::from))
}

fn resolve(override_path: Option<&Path>, candidates: &[PathBuf]) -> Option<PathBuf> {
    let executable =
        |path: &Path| path.is_absolute() && path.is_file() && super::sys::is_executable(path);
    match override_path {
        Some(path) => executable(path).then(|| path.to_path_buf()),
        None => candidates.iter().find(|path| executable(path)).cloned(),
    }
}

/// Read `PROVENANT_GIT` and resolve the selected executable.
pub fn git_executable() -> Option<PathBuf> {
    resolve_git_executable(std::env::var_os("PROVENANT_GIT").as_deref().map(Path::new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn repository_path_rejects_ambiguous_output() {
        for output in [
            b"".as_slice(),
            b"/tmp",
            b"\n",
            b"relative\n",
            b"/tmp\nother\n",
            b"/tmp\0x\n",
        ] {
            assert!(parse_repository_path(output).is_err());
        }
        assert_eq!(
            parse_repository_path(b"/tmp\n").unwrap(),
            fs::canonicalize("/tmp").unwrap()
        );
    }

    #[test]
    fn override_wins_and_invalid_override_does_not_fall_back() {
        let root = tempfile::tempdir().expect("temp dir");
        let override_git = root.path().join("override");
        fs::write(&override_git, b"git").expect("override binary");
        fs::set_permissions(&override_git, fs::Permissions::from_mode(0o700))
            .expect("override mode");
        assert_eq!(
            resolve_git_executable(Some(&override_git)),
            Some(override_git.clone())
        );
        assert_eq!(
            resolve_git_executable(Some(&root.path().join("missing"))),
            None
        );
        assert_eq!(
            resolve_git_executable(Some(Path::new("relative-git"))),
            None
        );
    }

    #[test]
    fn candidates_use_preference_order_and_require_an_executable_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::write(&first, b"git").expect("first binary");
        fs::write(&second, b"git").expect("second binary");
        fs::set_permissions(&first, fs::Permissions::from_mode(0o700)).expect("first mode");
        fs::set_permissions(&second, fs::Permissions::from_mode(0o700)).expect("second mode");
        assert_eq!(resolve(None, &[first.clone(), second.clone()]), Some(first));
        fs::set_permissions(&second, fs::Permissions::from_mode(0o600)).expect("second mode");
        fs::set_permissions(root.path().join("first"), fs::Permissions::from_mode(0o600))
            .expect("first mode");
        assert_eq!(resolve(None, &[root.path().join("first"), second]), None);
    }
}
