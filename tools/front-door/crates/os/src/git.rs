//! Host Git executable selection shared by every product owner.
use crate::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read},
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

/// A checkout marker, including paths only when its metadata is well formed.
/// The marker itself remains visible even when those paths cannot be read.
pub struct RepositoryMarker {
    pub git_dir: Option<PathBuf>,
    pub common_dir: Option<PathBuf>,
}

/// Inspect a checkout marker without consulting the inherited Git environment.
/// This is for guards that must recognise even an invalid or incomplete marker.
pub fn repository_marker(top: &Path) -> Option<RepositoryMarker> {
    let marker = top.join(".git");
    fs::symlink_metadata(&marker).ok()?;
    if fs::metadata(&marker).is_ok_and(|metadata| metadata.is_dir()) {
        return Some(RepositoryMarker {
            git_dir: Some(marker.clone()),
            common_dir: Some(marker),
        });
    }
    let git_dir = read_marker_line(&marker)
        .ok()
        .and_then(|line| {
            line.strip_prefix("gitdir:")
                .map(|path| path.trim().to_owned())
        })
        .filter(|line| !line.is_empty())
        .map(|line| top.join(line));
    let common_dir = git_dir.as_ref().map(|private| {
        read_marker_line(&private.join("commondir"))
            .ok()
            .filter(|line| !line.trim().is_empty())
            .map_or_else(|| private.clone(), |line| private.join(line.trim()))
    });
    Some(RepositoryMarker {
        git_dir,
        common_dir,
    })
}

fn read_marker_line(path: &Path) -> io::Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(4097).read_to_end(&mut bytes)?;
    let line = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    if bytes.len() > 4096 || line.is_empty() || line.contains(&0) || line.contains(&b'\n') {
        return Err(io::Error::other("invalid Git marker"));
    }
    String::from_utf8(line.to_vec()).map_err(io::Error::other)
}

/// Resolve and validate a repository path without inheriting Git redirects.
pub fn repository_path(cwd: &Path, field: RepositoryPath) -> io::Result<PathBuf> {
    let git = git_executable().ok_or_else(|| io::Error::other("Git executable unavailable"))?;
    let executor = SystemExecutor::new(1).map_err(io::Error::other)?;
    repository_path_with(&executor, &git, cwd, field)
}

/// Resolve a repository path with a caller-selected executable and executor.
/// The environment is rebuilt here so inherited Git redirects cannot affect
/// discovery, even when the caller later pins its own trusted metadata paths.
pub fn repository_path_with(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
    field: RepositoryPath,
) -> io::Result<PathBuf> {
    repository_path_with_cancel(executor, git, cwd, field, None)
}

/// As [`repository_path_with`], with cancellation for long-running workflows.
pub fn repository_path_with_cancel(
    executor: &dyn Executor,
    git: &Path,
    cwd: &Path,
    field: RepositoryPath,
    cancel: Option<&provenant_core::CancelToken>,
) -> io::Result<PathBuf> {
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
    options.cancel = cancel;
    options.stop_on_output_limit = true;
    let result = executor.execute(options).map_err(io::Error::other)?;
    if result.termination != Termination::Exit(0)
        || result.timed_out
        || result.cancelled
        || result.overflow_stopped
        || result.drain_truncated
        || result.stdout.truncated
        || result.stdout.read_failed
        || result.stdout.sink_failed
        || result.stdout.sink_lost_bytes != 0
        || result.stdout.oversized_frames != 0
        || result.stdout.decoder_lost_frames != 0
        || result.stdout.filter_failed
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
    use crate::exec::{CapturedStream, ExecError, ExecOutcome};
    use std::{fs, os::unix::fs::PermissionsExt};

    struct DiscoveryExecutor {
        output: Vec<u8>,
        truncated: bool,
    }

    impl Executor for DiscoveryExecutor {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let env = options.env.expect("isolated environment");
            for key in [
                "GIT_DIR",
                "GIT_WORK_TREE",
                "GIT_INDEX_FILE",
                "GIT_COMMON_DIR",
                "GIT_OBJECT_DIRECTORY",
            ] {
                assert!(!env.iter().any(|(name, _)| name == key));
            }
            assert!(
                options
                    .argv
                    .contains(&OsString::from("--path-format=absolute"))
            );
            assert!(options.stop_on_output_limit);
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream {
                    bytes: self.output.clone(),
                    truncated: self.truncated,
                    ..Default::default()
                },
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }

    #[test]
    fn repository_path_uses_isolated_environment_and_refuses_truncation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let output = format!("{}\n", dir.path().display()).into_bytes();
        let executor = DiscoveryExecutor {
            output: output.clone(),
            truncated: false,
        };
        for field in [RepositoryPath::Toplevel, RepositoryPath::CommonDir] {
            assert_eq!(
                repository_path_with(&executor, Path::new("/git"), dir.path(), field).unwrap(),
                fs::canonicalize(dir.path()).unwrap()
            );
        }
        let executor = DiscoveryExecutor {
            output,
            truncated: true,
        };
        assert!(
            repository_path_with(
                &executor,
                Path::new("/git"),
                dir.path(),
                RepositoryPath::CommonDir
            )
            .is_err()
        );
    }

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
    fn repository_path_identifies_linked_worktree() {
        let temp = tempfile::tempdir().expect("temp dir");
        let main = temp.path().join("main");
        let linked = temp.path().join("linked");
        let common = main.join(".git");
        let private = common.join("worktrees/linked");
        for dir in [
            &linked,
            &private,
            &common.join("objects"),
            &common.join("refs"),
        ] {
            fs::create_dir_all(dir).expect("fixture directory");
        }
        fs::write(common.join("HEAD"), "ref: refs/heads/main\n").expect("main HEAD");
        fs::write(private.join("HEAD"), "ref: refs/heads/linked\n").expect("linked HEAD");
        fs::write(private.join("commondir"), "../..\n").expect("common directory");
        fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", private.display()),
        )
        .expect("gitfile");
        for root in [&main, &linked] {
            assert_eq!(
                repository_path(root, RepositoryPath::Toplevel).unwrap(),
                fs::canonicalize(root).unwrap()
            );
            assert_eq!(
                repository_path(root, RepositoryPath::CommonDir).unwrap(),
                fs::canonicalize(&common).unwrap()
            );
        }
    }

    #[test]
    fn marker_discovery_keeps_invalid_markers_visible() {
        let temp = tempfile::tempdir().unwrap();
        let top = temp.path().join("checkout");
        let private = temp.path().join("private");
        fs::create_dir(&top).unwrap();
        fs::create_dir(&private).unwrap();
        fs::write(top.join(".git"), format!("gitdir: {}\n", private.display())).unwrap();
        fs::write(private.join("commondir"), "../common\n").unwrap();
        let marker = repository_marker(&top).unwrap();
        assert_eq!(marker.git_dir, Some(private.clone()));
        assert_eq!(marker.common_dir, Some(private.join("../common")));
        fs::write(private.join("commondir"), "../common").unwrap();
        assert_eq!(
            repository_marker(&top).unwrap().common_dir,
            Some(private.join("../common"))
        );
        fs::write(top.join(".git"), format!("gitdir:{}\n", private.display())).unwrap();
        fs::write(private.join("commondir"), b"bad\nextra\n").unwrap();
        let marker = repository_marker(&top).unwrap();
        assert_eq!(marker.git_dir, Some(private.clone()));
        assert_eq!(marker.common_dir, Some(private.clone()));
        fs::write(top.join(".git"), b"gitdir: bad\nextra\n").unwrap();
        let marker = repository_marker(&top).unwrap();
        assert!(marker.git_dir.is_none());
        assert!(marker.common_dir.is_none());
        fs::write(top.join(".git"), vec![b'x'; 4097]).unwrap();
        assert!(repository_marker(&top).unwrap().git_dir.is_none());
        assert!(repository_marker(&temp.path().join("missing")).is_none());
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
