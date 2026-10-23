//! Trusted path operations for diagnostic reads, run leases and marker writers.
//! Unlike custody operations, these explicitly follow final symlinks.
use super::*;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}
#[derive(Clone, Copy, Debug)]
pub struct LockOptions {
    pub mode: LockMode,
    /// None requests a blocking flock; Some bounds lock acquisition.
    pub timeout: Option<Duration>,
}
#[derive(Clone, Copy, Debug)]
pub struct AppendOptions {
    /// Python open(path, 'a+') uses 0666, filtered by the process umask.
    /// Existing file permissions are never changed.
    pub create_mode: u32,
}
impl Default for AppendOptions {
    fn default() -> Self {
        Self { create_mode: 0o666 }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ReadOptions {
    pub limit: u64,
}

impl FileHandle {
    pub fn open_append(path: &Path, options: AppendOptions) -> Result<Self> {
        let mut file = Self::regular(
            os::open_append(path, options.create_mode).map_err(error("open append", path))?,
            path,
        )?;
        file.seek(SeekFrom::End(0))
            .map_err(error("seek append", path))?;
        Ok(file)
    }
    pub fn lock(&self, options: LockOptions) -> Result<()> {
        let Some(timeout) = options.timeout else {
            return os::flock(&self.file, options.mode, false).map_err(error("flock", &self.path));
        };
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| FsError::Io {
                operation: "flock deadline",
                path: self.path.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "lock timeout outside clock range",
                ),
            })?;
        loop {
            match os::flock(&self.file, options.mode, true) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(source) => return Err(error("flock", &self.path)(source)),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(FsError::LockTimeout {
                    path: self.path.clone(),
                    timeout,
                });
            }
            std::thread::sleep(remaining.min(Duration::from_millis(5)));
        }
    }
}
/// Read-only path access follows links, checks regular-file type before reading,
/// and consumes at most N bytes. Held-file lengths before/after the read detect
/// oversize; mismatched length/read counts are refused (not a snapshot promise).
pub fn read_following(path: &Path, options: ReadOptions) -> Result<Vec<u8>> {
    let mut file = FileHandle::regular(
        os::open_following(path).map_err(error("open following", path))?,
        path,
    )?;
    if file.metadata()?.size > options.limit {
        return Err(FsError::TooLarge {
            limit: options.limit,
        });
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(options.limit)
        .read_to_end(&mut bytes)
        .map_err(error("read following", path))?;
    let after = file.metadata()?.size;
    if after > options.limit {
        return Err(FsError::TooLarge {
            limit: options.limit,
        });
    }
    if after != bytes.len() as u64 {
        return Err(FsError::RaceRefused {
            name: path.as_os_str().into(),
        });
    }
    Ok(bytes)
}
/// Injectable initial path owner. Consumers retain their own lease/marker policy.
pub trait PathIo: Send + Sync {
    fn open_append(&self, path: &Path, options: AppendOptions) -> Result<Box<dyn AppendFile>>;
    fn read_following(&self, path: &Path, options: ReadOptions) -> Result<Vec<u8>>;
    fn publish_marker(&self, path: &Path, bytes: &[u8]) -> Result<()>;
}
pub trait AppendFile: Read + Write + Seek + Send + Sync {
    fn lock(&self, options: LockOptions) -> Result<()>;
    fn unlock(&self) -> Result<()>;
    fn sync(&self) -> Result<()>;
}
impl AppendFile for FileHandle {
    fn lock(&self, options: LockOptions) -> Result<()> {
        FileHandle::lock(self, options)
    }
    fn unlock(&self) -> Result<()> {
        FileHandle::unlock(self)
    }
    fn sync(&self) -> Result<()> {
        FileHandle::sync(self)
    }
}
impl PathIo for LocalFs {
    fn open_append(&self, path: &Path, options: AppendOptions) -> Result<Box<dyn AppendFile>> {
        Ok(Box::new(FileHandle::open_append(path, options)?))
    }
    fn read_following(&self, path: &Path, options: ReadOptions) -> Result<Vec<u8>> {
        read_following(path, options)
    }
    fn publish_marker(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        publish_marker(path, bytes)
    }
}
/// Durable replacement: exclusive temporary 0600 file, file fsync, rename, then
/// directory fsync. The existing atomic_write capability owns all effects.
pub fn publish_marker(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path.file_name().ok_or_else(|| FsError::InvalidComponent {
        name: path.as_os_str().into(),
    })?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Directory::open_anchor(parent)?.atomic_write(name, ".marker-", bytes, PublishMode::Replace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[test]
    fn append_preserves_contents_and_existing_mode_and_read_follows_links() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lease");
        let mut file =
            FileHandle::open_append(&path, AppendOptions { create_mode: 0o600 }).unwrap();
        file.write_all(b"first").unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"firstsecond");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let mut reopened = FileHandle::open_append(&path, AppendOptions::default()).unwrap();
        let mut at_end = Vec::new();
        reopened.read_to_end(&mut at_end).unwrap();
        assert!(at_end.is_empty(), "Python a+ starts its read cursor at EOF");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert_eq!(
            read_following(&link, ReadOptions { limit: 11 }).unwrap(),
            b"firstsecond"
        );
        assert!(matches!(
            read_following(&link, ReadOptions { limit: 10 }),
            Err(FsError::TooLarge { limit: 10 })
        ));
        assert!(matches!(
            read_following(&root.path().join("absent"), ReadOptions { limit: 10 }),
            Err(FsError::Io { .. })
        ));
    }
    #[test]
    fn shared_locks_coexist_and_exclusive_lock_times_out_then_releases() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lease");
        let first = FileHandle::open_append(&path, AppendOptions::default()).unwrap();
        let second = FileHandle::open_append(&path, AppendOptions::default()).unwrap();
        let shared = LockOptions {
            mode: LockMode::Shared,
            timeout: Some(Duration::ZERO),
        };
        first.lock(shared).unwrap();
        second.lock(shared).unwrap();
        let exclusive = LockOptions {
            mode: LockMode::Exclusive,
            timeout: Some(Duration::ZERO),
        };
        assert!(matches!(
            second.lock(exclusive),
            Err(FsError::LockTimeout { .. })
        ));
        first.unlock().unwrap();
        second
            .lock(LockOptions {
                mode: LockMode::Exclusive,
                timeout: None,
            })
            .unwrap();
        assert!(matches!(
            first.lock(shared),
            Err(FsError::LockTimeout { .. })
        ));
        second.unlock().unwrap();
        first.lock(exclusive).unwrap();
    }
    #[test]
    fn marker_reuses_durable_replacement_and_does_not_follow_destination_link() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, b"old").unwrap();
        let marker = root.path().join("marker");
        symlink(&target, &marker).unwrap();
        LocalFs.publish_marker(&marker, b"new").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(std::fs::read(&marker).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }
}
