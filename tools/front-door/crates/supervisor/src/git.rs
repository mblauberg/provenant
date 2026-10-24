//! Host Git executable selection shared by every product owner.
use std::path::{Path, PathBuf};

const CANDIDATES: [&str; 5] = [
    "/opt/homebrew/bin/git",
    "/usr/local/bin/git",
    "/opt/local/bin/git",
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
