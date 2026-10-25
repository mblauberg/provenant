pub(crate) fn tempdir() -> std::io::Result<tempfile::TempDir> {
    // Canonical roots avoid macOS temp aliases while honouring the lane's writable TMPDIR.
    tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir())?)
}

pub(crate) fn short_tempdir(socket_name: &str) -> std::io::Result<tempfile::TempDir> {
    use std::{fs, io, os::unix::ffi::OsStrExt};

    // Unix socket names need a short root; lanes may deny writes to the usual /tmp.
    let short = fs::canonicalize("/tmp").and_then(tempfile::tempdir_in);
    let temp = match short {
        Ok(temp) => temp,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            tempdir()?
        }
        Err(error) => return Err(error),
    };
    let socket = temp.path().join(socket_name);
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
