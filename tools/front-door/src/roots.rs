//! Read-only edge resolution. No provider, database, or legacy-content reads.
use provenant_core::{PathResolver, RootInputs, Roots};
use provenant_supervisor::exec::{
    ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
};
use std::{
    ffi::{OsStr, OsString},
    fs, io,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum RootError {
    #[error("cannot resolve executable or root")]
    Io(#[from] io::Error),
    #[error("uninstalled binary requires an absolute directory in PROVENANT_ROOT")]
    Uninstalled,
    #[error("HOME must be an absolute directory")]
    Home,
    #[error("project must be a Git checkout")]
    Checkout,
    #[error("invalid state root: {0}")]
    State(#[from] provenant_core::RootError),
}
pub fn product_root() -> Result<PathBuf, RootError> {
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let parent = executable.parent().ok_or(RootError::Uninstalled)?;
    if parent.file_name() == Some(OsStr::new("bin")) {
        let candidate = parent.parent().ok_or(RootError::Uninstalled)?;
        if candidate.join("release.json").is_file() {
            return Ok(candidate.into());
        }
    }
    let root = PathBuf::from(std::env::var_os("PROVENANT_ROOT").ok_or(RootError::Uninstalled)?);
    if !root.is_absolute() || !root.is_dir() {
        return Err(RootError::Uninstalled);
    }
    Ok(fs::canonicalize(root)?)
}
/// The Git binary: `PROVENANT_GIT` when set, otherwise the first executable in a fixed
/// preference list. PATH is not searched: Apple's `/usr/bin/git` lacks `--attr-source`, so a
/// newer package-manager Git must win when both exist.
pub(crate) fn git_binary() -> Option<PathBuf> {
    resolve_git_binary(
        std::env::var_os("PROVENANT_GIT")
            .map(PathBuf::from)
            .as_deref(),
        &GIT_CANDIDATES.map(PathBuf::from),
    )
}
const GIT_CANDIDATES: [&str; 5] = [
    "/opt/homebrew/bin/git",
    "/usr/local/bin/git",
    "/opt/local/bin/git",
    "/usr/bin/git",
    "/bin/git",
];
pub(crate) fn resolve_git_binary(
    override_path: Option<&Path>,
    candidates: &[PathBuf],
) -> Option<PathBuf> {
    let executable = |path: &Path| {
        path.is_absolute() && path.is_file() && provenant_supervisor::sys::is_executable(path)
    };
    match override_path {
        Some(path) => executable(path).then(|| path.to_path_buf()),
        None => candidates.iter().find(|path| executable(path)).cloned(),
    }
}

pub fn selected_project(selected: Option<&Path>, check: bool) -> Result<PathBuf, RootError> {
    let path = match selected {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()?,
    };
    // A selected path that does not resolve is the caller's fault.
    let path = fs::canonicalize(path).map_err(|error| match selected {
        Some(_) => RootError::Checkout,
        None => error.into(),
    })?;
    if !path.is_dir() {
        return Err(RootError::Checkout);
    }
    if check || selected.is_some() {
        // Explicit projects and checks require a real checkout. Default root
        // reporting remains process-free.
        // The isolated Git environment ignores inherited redirects and config.
        let root = git_checkout(&path)?;
        return Ok(root);
    }
    Ok(path)
}
fn git_checkout(path: &Path) -> Result<PathBuf, RootError> {
    let git = git_binary().ok_or(RootError::Checkout)?;
    let argv: Vec<OsString> = [
        git.as_os_str(),
        OsStr::new("--no-pager"),
        OsStr::new("--no-optional-locks"),
        OsStr::new("-c"),
        OsStr::new("core.fsmonitor=false"),
        OsStr::new("-c"),
        OsStr::new("core.hooksPath=/dev/null"),
        OsStr::new("rev-parse"),
        OsStr::new("--show-toplevel"),
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
    let mut options = ExecOptions::new(&argv, path, Duration::from_secs(2));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Discard;
    options.stop_on_output_limit = true;
    let result = SystemExecutor::new(1)
        .and_then(|executor| executor.execute(options))
        .map_err(|_| RootError::Checkout)?;
    if result.termination != Termination::Exit(0)
        || result.timed_out
        || result.drain_truncated
        || result.stdout.truncated
        || result.stdout.read_failed
    {
        return Err(RootError::Checkout);
    }
    let bytes = result
        .stdout
        .bytes
        .strip_suffix(b"\n")
        .ok_or(RootError::Checkout)?;
    if bytes.is_empty() || bytes.contains(&0) || bytes.contains(&b'\n') {
        return Err(RootError::Checkout);
    }
    let root = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    if !root.is_absolute() {
        return Err(RootError::Checkout);
    }
    Ok(fs::canonicalize(root)?)
}
/// Validate caller-selected roots before composing any leaf. This creates no
/// state and does not require a product installation for an unavailable owner.
pub fn validate_globals(globals: &provenant_cli::Globals) -> Result<(), RootError> {
    let project = if globals.project.is_some() {
        Some(selected_project(globals.project.as_deref(), false)?)
    } else {
        None
    };
    if globals.state_dir.is_some() {
        let project = project.unwrap_or(std::env::current_dir()?);
        let assets = product_root().unwrap_or_else(|_| project.clone());
        roots(globals, &assets, &project)?;
    }
    Ok(())
}
struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
        let mut ancestor = path;
        let mut missing = Vec::new();
        loop {
            // Distinguish a missing component from an unreadable/dangling link.
            match fs::symlink_metadata(ancestor) {
                Ok(_) => {
                    let mut bound = fs::canonicalize(ancestor)?;
                    for tail in missing.iter().rev() {
                        bound.push(tail);
                    }
                    return Ok(bound);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    missing.push(
                        ancestor
                            .file_name()
                            .ok_or_else(|| io::Error::other("missing root"))?
                            .to_owned(),
                    );
                    ancestor = ancestor
                        .parent()
                        .ok_or_else(|| io::Error::other("missing parent"))?;
                }
                Err(error) => return Err(error),
            }
        }
    }
}
pub fn roots(
    globals: &provenant_cli::Globals,
    assets: &Path,
    project: &Path,
) -> Result<Roots, RootError> {
    let home = home()?;
    let mut inputs = RootInputs::new(home, assets.into());
    inputs.state_dir = globals.state_dir.clone();
    inputs.environment_state_dir = std::env::var_os("PROVENANT_STATE_DIR").map(PathBuf::from);
    inputs.workspaces.push(project.into());
    Ok(inputs.resolve(&Resolver)?)
}

/// Canonical home used by root selection and home-relative Fabric identity.
pub fn home() -> Result<PathBuf, RootError> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or(RootError::Home)?);
    if !home.is_absolute() {
        return Err(RootError::Home);
    }
    Ok(fs::canonicalize(home)?)
}
