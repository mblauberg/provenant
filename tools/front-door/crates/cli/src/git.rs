//! Host Git selection shared by command and lane composition.
use std::path::{Path, PathBuf};

/// The Git binary: `PROVENANT_GIT` when set, otherwise the first executable in a fixed
/// preference list. PATH is not searched: Apple's `/usr/bin/git` lacks `--attr-source`, so a
/// newer package-manager Git must win when both exist.
pub fn git_binary() -> Option<PathBuf> {
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
pub fn resolve_git_binary(override_path: Option<&Path>, candidates: &[PathBuf]) -> Option<PathBuf> {
    let executable = |path: &Path| {
        path.is_absolute() && path.is_file() && provenant_supervisor::sys::is_executable(path)
    };
    match override_path {
        Some(path) => executable(path).then(|| path.to_path_buf()),
        None => candidates.iter().find(|path| executable(path)).cloned(),
    }
}
