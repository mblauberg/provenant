//! Shared filesystem fixture roots.
use std::{io, path::Path};

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
