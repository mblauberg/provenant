//! Safe descriptor-relative filesystem capabilities for custody and publication.
//!
//! An anchor is opened once from a trusted path. Every operation after that uses
//! its held descriptor and exactly one normal component; renaming the anchor or
//! replacing an ancestor with a symlink cannot redirect an operation. Callers
//! retain responsibility for directory ownership and permissions: an attacker
//! with write access to a held directory can replace entries between syscalls.
//! No check-then-unlink or conditional-rename atomicity is promised.
use crate::sys::fs as os;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FsError {
    #[error("expected one normal relative path component: {name:?}")]
    InvalidComponent { name: OsString },
    #[error("filesystem {operation} failed for {path:?}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("entry changed identity or crossed the anchor device: {name:?}")]
    RaceRefused { name: OsString },
    #[error("entry is not a regular file: {name:?}")]
    NotRegular { name: OsString },
    #[error("file exceeds byte limit {limit}")]
    TooLarge { limit: u64 },
}
pub type Result<T> = std::result::Result<T, FsError>;

impl FsError {
    pub fn raw_os_error(&self) -> Option<i32> {
        match self {
            Self::Io { source, .. } => source.raw_os_error(),
            _ => None,
        }
    }
}
fn error<'a>(operation: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> FsError + 'a {
    move |source| FsError::Io {
        operation,
        path: path.into(),
        source,
    }
}
fn component(name: &OsStr) -> Result<&OsStr> {
    let bytes = name.as_bytes();
    // Check bytes, not Path::components(): that API normalises `a/.` and `a//`.
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
    {
        return Err(FsError::InvalidComponent { name: name.into() });
    }
    Ok(name)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub kind: FileKind,
    pub device: u64,
    pub inode: u64,
    pub links: u64,
    pub size: u64,
    pub allocated: u64,
    pub modified_ns: i128,
    pub uid: u32,
    pub mode: u32,
}
impl Metadata {
    pub(crate) fn from_std(m: std::fs::Metadata) -> Self {
        Self {
            kind: if m.is_file() {
                FileKind::Regular
            } else if m.is_dir() {
                FileKind::Directory
            } else if m.file_type().is_symlink() {
                FileKind::Symlink
            } else {
                FileKind::Other
            },
            device: m.dev(),
            inode: m.ino(),
            links: m.nlink(),
            size: m.size(),
            allocated: m.blocks().saturating_mul(512),
            modified_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
            uid: m.uid(),
            mode: m.mode(),
        }
    }
    pub fn same_identity(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

/// Owning directory handle. It deliberately implements neither AsRawFd nor Deref.
#[derive(Debug)]
pub struct Directory {
    file: File,
    device: u64,
}
impl Directory {
    /// The initial path is trusted; symlinks in ancestors (e.g. macOS /var) are
    /// permitted. The final component must be a directory, never a symlink.
    pub fn open_anchor(path: &Path) -> Result<Self> {
        Self::from_file(os::open_anchor(path).map_err(error("open anchor", path))?)
    }
    /// Open / or . without resolving a held directory back to a pathname.
    pub fn root(absolute: bool) -> Result<Self> {
        Self::open_anchor(Path::new(if absolute { "/" } else { "." }))
    }
    fn from_file(file: File) -> Result<Self> {
        let device = file
            .metadata()
            .map_err(error("fstat directory", Path::new("<directory>")))?
            .dev();
        Ok(Self { file, device })
    }
    pub fn metadata(&self) -> Result<Metadata> {
        self.file
            .metadata()
            .map(Metadata::from_std)
            .map_err(error("fstat directory", Path::new("<directory>")))
    }
    /// Stat a single entry. Following a final link requires explicit authority
    /// from the caller; it does not affect the default no-follow open methods.
    pub fn metadata_at(&self, name: &OsStr, follow: bool) -> Result<Metadata> {
        let name = component(name)?;
        os::metadata_at(&self.file, name, follow).map_err(error("fstatat", Path::new(name)))
    }
    pub fn open_directory(&self, name: &OsStr) -> Result<Self> {
        self.open_directory_with_links(name, false)
    }
    /// Explicitly permit following the final symlink for callers whose path
    /// policy authorises it (e.g. the confinement resolver's link expansion).
    pub fn open_directory_with_links(&self, name: &OsStr, follow: bool) -> Result<Self> {
        let name = component(name)?;
        let mut child = Self::from_file(
            os::open_directory(&self.file, name, follow)
                .map_err(error("openat directory", Path::new(name)))?,
        )?;
        child.device = self.device;
        Ok(child)
    }
    /// Refuse replacement between stat/open/stat and cross-device traversal.
    /// The returned handle keeps that verified identity even after a rename.
    pub fn open_verified_directory(&self, name: &OsStr) -> Result<Self> {
        let before = self.metadata_at(name, false)?;
        let child = self.open_directory(name)?;
        let opened = child.metadata()?;
        let after = self.metadata_at(name, false)?;
        if before.kind != FileKind::Directory
            || !before.same_identity(&opened)
            || !opened.same_identity(&after)
            || opened.device != self.device
        {
            return Err(FsError::RaceRefused { name: name.into() });
        }
        Ok(child)
    }
    pub fn open_read(&self, name: &OsStr) -> Result<FileHandle> {
        let name = component(name)?;
        FileHandle::regular(
            os::open_read(&self.file, name).map_err(error("openat read", Path::new(name)))?,
            name,
        )
    }
    /// Exclusive create, mode 0600 subject to umask, O_NOFOLLOW | O_CLOEXEC.
    pub fn create_new(&self, name: &OsStr) -> Result<FileHandle> {
        let name = component(name)?;
        FileHandle::regular(
            os::create_new(&self.file, name).map_err(error("openat create", Path::new(name)))?,
            name,
        )
    }
    pub fn mkdir(&self, name: &OsStr, mode: u32) -> Result<()> {
        let name = component(name)?;
        os::mkdir(&self.file, name, mode).map_err(error("mkdirat", Path::new(name)))
    }
    pub fn unlink(&self, name: &OsStr) -> Result<()> {
        self.remove(name, false)
    }
    pub fn rmdir(&self, name: &OsStr) -> Result<()> {
        self.remove(name, true)
    }
    fn remove(&self, name: &OsStr, directory: bool) -> Result<()> {
        let name = component(name)?;
        os::unlink(&self.file, name, directory).map_err(error("unlinkat", Path::new(name)))
    }
    /// Atomic same-filesystem publication; existing destinations are refused.
    /// This never falls back to a check-then-rename sequence.
    pub fn rename_noreplace(
        &self,
        source: &OsStr,
        destination_dir: &Self,
        destination: &OsStr,
    ) -> Result<()> {
        self.rename(source, destination_dir, destination, os::Rename::NoReplace)
    }
    pub fn exchange(
        &self,
        source: &OsStr,
        destination_dir: &Self,
        destination: &OsStr,
    ) -> Result<()> {
        self.rename(source, destination_dir, destination, os::Rename::Exchange)
    }
    pub fn replace(
        &self,
        source: &OsStr,
        destination_dir: &Self,
        destination: &OsStr,
    ) -> Result<()> {
        self.rename(source, destination_dir, destination, os::Rename::Replace)
    }
    fn rename(
        &self,
        source: &OsStr,
        destination_dir: &Self,
        destination: &OsStr,
        mode: os::Rename,
    ) -> Result<()> {
        component(source)?;
        component(destination)?;
        os::rename(&self.file, source, &destination_dir.file, destination, mode)
            .map_err(error("renameat", Path::new(destination)))
    }
    pub fn symlink(&self, target: &Path, name: &OsStr) -> Result<()> {
        let name = component(name)?;
        os::symlink(target, &self.file, name).map_err(error("symlinkat", Path::new(name)))
    }
    pub fn sync(&self) -> Result<()> {
        self.file
            .sync_all()
            .map_err(error("fsync directory", Path::new("<directory>")))
    }
    pub fn make_writable(&self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let permissions = self
            .file
            .metadata()
            .map_err(error("fstat directory", Path::new("<directory>")))?
            .permissions();
        self.file
            .set_permissions(std::fs::Permissions::from_mode(permissions.mode() | 0o700))
            .map_err(error("fchmod directory", Path::new("<directory>")))
    }
    /// A fresh descriptor stream on each call, independent of prior enumeration.
    pub fn entries(&self) -> Result<Vec<OsString>> {
        os::entries(&self.file).map_err(error("readdir", Path::new("<directory>")))
    }
    pub fn read_bounded(&self, name: &OsStr, limit: u64) -> Result<Vec<u8>> {
        self.open_read(name)?.read_bounded(limit)
    }
}

/// File capability; no raw owning descriptor escapes the supervisor boundary.
#[derive(Debug)]
pub struct FileHandle(File);
impl FileHandle {
    fn regular(file: File, name: &OsStr) -> Result<Self> {
        if !file
            .metadata()
            .map_err(error("fstat file", Path::new(name)))?
            .is_file()
        {
            return Err(FsError::NotRegular { name: name.into() });
        }
        Ok(Self(file))
    }
    /// Trusted initial source path; O_NOFOLLOW applies to its final component.
    pub fn open_source(path: &Path) -> Result<Self> {
        Self::regular(
            os::open_source(path).map_err(error("open source", path))?,
            path.as_os_str(),
        )
    }
    /// Like dup: shared file offset; descriptor remains close-on-exec.
    pub fn duplicate(&self) -> Result<Self> {
        self.0
            .try_clone()
            .map(Self)
            .map_err(error("duplicate file", Path::new("<file>")))
    }
    pub fn metadata(&self) -> Result<Metadata> {
        self.0
            .metadata()
            .map(Metadata::from_std)
            .map_err(error("fstat file", Path::new("<file>")))
    }
    pub fn sync(&self) -> Result<()> {
        self.0
            .sync_all()
            .map_err(error("fsync file", Path::new("<file>")))
    }
    /// Read at most limit+1 bytes from the current offset, then refuse oversize.
    /// Metadata alone is never used to enforce the cap (files can grow).
    pub fn read_bounded(&mut self, limit: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        (&mut self.0)
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(error("read file", Path::new("<file>")))?;
        if bytes.len() as u64 > limit {
            return Err(FsError::TooLarge { limit });
        }
        Ok(bytes)
    }
    /// Nonblocking lock acquisition; callers own their bounded retry/deadline.
    pub fn try_lock(&self) -> Result<bool> {
        match self.0.try_lock() {
            Ok(()) => Ok(true),
            Err(std::fs::TryLockError::WouldBlock) => Ok(false),
            Err(std::fs::TryLockError::Error(e)) => Err(error("lock file", Path::new("<file>"))(e)),
        }
    }
    pub fn unlock(&self) -> Result<()> {
        self.0
            .unlock()
            .map_err(error("unlock file", Path::new("<file>")))
    }
}
impl Read for FileHandle {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}
impl Write for FileHandle {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
impl Seek for FileHandle {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        self.0.seek(p)
    }
}

/// OS entropy, no global counter or clock; callers use exclusive creation too.
pub fn temporary_name() -> Result<OsString> {
    let bytes = os::entropy().map_err(error("getentropy", Path::new("<temporary name>")))?;
    let mut name = String::from(".cf-dispatch-output.");
    for byte in bytes {
        name.push_str(&format!("{byte:02x}"));
    }
    Ok(name.into())
}

#[cfg(test)]
mod tests;
