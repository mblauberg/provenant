//! Explicit trusted-path operations for append-only output and bounded reads.
//! Lock custody uses held `SafeDir` capabilities rather than path handles.
use super::*;

#[derive(Clone, Copy, Debug)]
pub struct AppendOptions {
    /// Creation mode for a new file, filtered by the process umask.
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
/// Injectable initial path owner. Consumers retain their own append policy;
/// publication reports visible-but-unsynced entries as a successful status.
pub trait PathIo: Send + Sync {
    fn open_append(&self, path: &Path, options: AppendOptions) -> Result<Box<dyn AppendFile>>;
    fn read_following(&self, path: &Path, options: ReadOptions) -> Result<Vec<u8>>;
    fn publish_marker(&self, path: &Path, bytes: &[u8]) -> Result<Publication>;
}
pub trait AppendFile: Read + Write + Seek + Send + Sync {
    fn sync(&self) -> Result<()>;
}
impl AppendFile for FileHandle {
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
    fn publish_marker(&self, path: &Path, bytes: &[u8]) -> Result<Publication> {
        publish_marker(path, bytes)
    }
}
/// Durable replacement: exclusive temporary 0600 file, file fsync, rename, then
/// directory fsync. The held-directory capability owns all effects.
pub fn publish_marker(path: &Path, bytes: &[u8]) -> Result<Publication> {
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
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn append_preserves_existing_mode_and_bounded_read_follows_links() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("output");
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
        assert!(at_end.is_empty());
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
    fn marker_returns_durable_after_replace_without_following_destination_link() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, b"old").unwrap();
        let marker = root.path().join("marker");
        symlink(&target, &marker).unwrap();
        assert!(matches!(
            LocalFs.publish_marker(&marker, b"new").unwrap(),
            Publication::Durable
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(std::fs::read(&marker).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }
}
