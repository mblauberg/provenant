use std::{fs, io, os::unix::ffi::OsStrExt};

pub(crate) fn tempdir(prefix: &str) -> io::Result<tempfile::TempDir> {
    // Canonical roots avoid macOS temp aliases while honouring the lane's writable TMPDIR.
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(fs::canonicalize(std::env::temp_dir())?)
}

pub(crate) fn short_tempdir(prefix: &str, socket_name: &str) -> io::Result<tempfile::TempDir> {
    // Unix socket names need a short root; lanes may deny writes to the usual /tmp.
    let short = fs::canonicalize("/tmp")
        .and_then(|root| tempfile::Builder::new().prefix(prefix).tempdir_in(root));
    let temp = match short {
        Ok(temp) => temp,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            tempdir(prefix)?
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
