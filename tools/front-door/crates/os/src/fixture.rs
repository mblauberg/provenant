//! Shared filesystem fixture roots.
use std::{io, path::Path};

#[path = "fixture_git.rs"]
mod fixture_git;
pub use fixture_git::{
    fixture_git_settings, git_command, git_command_at, git_env, run_git, scrub_git_env,
};

/// Run once on this thread after an existing lock has contended and before
/// its bounded wait pauses. A nonblocking try never invokes this hook.
pub fn on_existing_lock_wait(hook: impl FnOnce() + 'static) {
    crate::fs::EXISTING_LOCK_WAIT_HOOK.with(|pending| {
        assert!(pending.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

/// Create a fixture state root with the private permissions required by stores.
///
/// Missing parents are created private too, and the leaf is set explicitly, so
/// fixtures are independent of the process umask.
pub fn private_state_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Build the libtest name for the current module and a child test.
///
/// Re-exec fixtures pass this to `--exact`; spelling a module path by hand
/// lets a module rename silently turn a child run into a zero-test success.
pub fn exact_test_path(module: &str, test: &str) -> String {
    let module = module.split_once("::").map_or("", |(_, path)| path);
    if module.is_empty() {
        test.to_owned()
    } else {
        format!("{module}::{test}")
    }
}

/// Create a temporary directory beneath the canonical environment temporary root.
///
/// `create` makes the directory inside the root it is given, so callers keep
/// `tempfile` as a dev-dependency (ADR 0028).
pub fn tempdir<T>(create: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    // Canonical roots avoid macOS temp aliases while honouring the lane's writable TMPDIR.
    create(&std::fs::canonicalize(std::env::temp_dir())?)
}

/// Create a temporary directory with room for a Unix socket path.
///
/// `socket_name` is the socket file name that will be joined to the returned
/// directory. It is used only to check the path against the platform limit of
/// 103 bytes on macOS and 107 bytes on Linux. No socket is created.
pub fn short_tempdir<T: AsRef<Path>>(
    socket_name: &str,
    create: impl Fn(&Path) -> io::Result<T>,
) -> io::Result<T> {
    use std::{fs, os::unix::ffi::OsStrExt};

    // Unix socket names need a short root; lanes may deny writes to the usual /tmp.
    let short = fs::canonicalize("/tmp").and_then(|root| create(&root));
    let temp = match short {
        Ok(temp) => temp,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            // A lane's TMPDIR is a short alias of its long attempt directory;
            // canonicalising it would restore the long path.
            create(&std::env::temp_dir())?
        }
        Err(error) => return Err(error),
    };
    let socket = temp.as_ref().join(socket_name);
    let bytes = socket.as_os_str().as_bytes().len();
    let capacity = if cfg!(target_os = "macos") { 104 } else { 108 };
    if bytes >= capacity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "fixture Unix socket path is {bytes} bytes; maximum is {}: {}",
                capacity - 1,
                socket.display()
            ),
        ));
    }
    Ok(temp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_test_path_matches_libtest_names_for_library_and_integration_crates() {
        let cases = [
            (
                "provenant_os::fixture::tests",
                "git_fixture_ignores_lane_environment",
                "fixture::tests::git_fixture_ignores_lane_environment",
            ),
            ("provider_auth_setup", "child_reexec", "child_reexec"),
            (
                "provider_auth_setup::tests",
                "child_reexec",
                "tests::child_reexec",
            ),
        ];
        for (module, test, expected) in cases {
            assert_eq!(exact_test_path(module, test), expected);
        }
    }

    #[test]
    fn tempdir_uses_canonical_environment_root() -> io::Result<()> {
        let root = std::fs::canonicalize(std::env::temp_dir())?;
        let temp = tempdir(|root| tempfile::Builder::new().prefix("fixture-").tempdir_in(root))?;
        assert_eq!(temp.path().parent(), Some(root.as_path()));
        Ok(())
    }

    #[test]
    fn short_tempdir_rejects_oversize_socket_name() {
        let error = short_tempdir(&"s".repeat(200), |root| tempfile::tempdir_in(root)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
