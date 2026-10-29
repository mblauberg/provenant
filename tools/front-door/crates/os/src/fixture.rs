//! Shared filesystem fixture roots.
use std::{io, path::Path};

#[path = "fixture_git.rs"]
mod fixture_git;
pub use fixture_git::{git_command, git_command_at, scrub_git_env};

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
            tempdir(&create)?
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
    #[allow(clippy::disallowed_methods)]
    fn git_fixture_ignores_lane_environment_and_path_wrapper()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::{fs, os::unix::fs::PermissionsExt, process::Command};

        fn run(dir: &Path, args: &[&str]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            let output = git_command().current_dir(dir).args(args).output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(output.stdout)
        }
        fn initialise(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
            run(dir, &["init", "-q"])?;
            run(
                dir,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "fixture",
                ],
            )?;
            Ok(())
        }

        if let Some(root) = std::env::var_os("PROVENANT_FIXTURE_GIT_CHILD") {
            let fixture = Path::new(&root).join("fixture");
            fs::create_dir(&fixture)?;
            initialise(&fixture)?;
            assert!(fixture.join(".git/HEAD").is_file());
            return Ok(());
        }

        let temp = tempfile::tempdir()?;
        let lane = temp.path().join("lane");
        let shim = temp.path().join("shim");
        fs::create_dir(&lane)?;
        fs::create_dir(&shim)?;
        initialise(&lane)?;
        let head = run(&lane, &["rev-parse", "HEAD"])?;
        let refs = run(
            &lane,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )?;
        let index = fs::read(lane.join(".git/index"))?;
        let config = fs::read(lane.join(".git/config"))?;
        fs::write(shim.join("git"), "#!/bin/sh\nexit 99\n")?;
        fs::set_permissions(shim.join("git"), fs::Permissions::from_mode(0o700))?;
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "fixture::tests::git_fixture_ignores_lane_environment_and_path_wrapper",
            ])
            .env("PROVENANT_FIXTURE_GIT_CHILD", temp.path())
            .env("PATH", &shim)
            .env("GIT_DIR", lane.join(".git"))
            .env("GIT_WORK_TREE", &lane)
            .env("GIT_COMMON_DIR", lane.join(".git"))
            .env("GIT_INDEX_FILE", lane.join(".git/index"))
            .env("GIT_OBJECT_DIRECTORY", lane.join(".git/objects"))
            .env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                lane.join(".git/objects"),
            )
            .env("GIT_NAMESPACE", "poison")
            .env("GIT_CEILING_DIRECTORIES", temp.path())
            .status()?;
        assert!(status.success());
        assert_eq!(head, run(&lane, &["rev-parse", "HEAD"])?);
        assert_eq!(
            refs,
            run(
                &lane,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            )?
        );
        assert_eq!(index, fs::read(lane.join(".git/index"))?);
        assert_eq!(config, fs::read(lane.join(".git/config"))?);
        Ok(())
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
