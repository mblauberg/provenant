//! Private safe OS operations until the supervisor crate exposes them.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// port: belongs in provenant-supervisor
pub(crate) fn atomic_write(destination: &Path, content: &[u8], prefix: &str) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("destination has no parent"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..128 {
        let temporary = parent.join(format!(
            "{prefix}{}-{stamp}-{attempt}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let result = (|| {
            file.write_all(content)?;
            file.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            drop(file);
            fs::rename(&temporary, destination)
        })();
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if result.is_ok() => return Err(error),
            Err(_) => {}
        }
        return result;
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "cannot allocate atomic-write temporary",
    ))
}
