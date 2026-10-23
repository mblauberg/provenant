//! Safe descriptor-relative filesystem capabilities for custody and publication.
//!
//! An anchor is opened once from a trusted path. Every operation after that uses
//! its held descriptor and exactly one normal component; renaming the anchor or
//! replacing an ancestor with a symlink cannot redirect an operation. Callers
//! retain responsibility for directory ownership and permissions: an attacker
//! with write access to a held directory can replace entries between syscalls.
//! No check-then-unlink or conditional-rename atomicity is promised.
use crate::sys::{errno, fs as os};
use provenant_core::{CancelToken, Clock, WaitBudget};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
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
    #[error("filesystem rename failed from {from_path:?} to {to_path:?}")]
    Rename {
        from_path: PathBuf,
        to_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("symlink or link loop refused for {path:?}")]
    Symlink {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("entry crossed the anchor device: {path:?}")]
    CrossDevice { path: PathBuf },
    #[error("entry changed identity: {name:?}")]
    RaceRefused { name: OsString },
    #[error("entry is not a regular file: {name:?}")]
    NotRegular { name: OsString },
    #[error("lock acquisition timed out for {path:?} after {timeout:?}")]
    LockTimeout { path: PathBuf, timeout: Duration },
    #[error("lock acquisition was cancelled for {path:?}")]
    Cancelled { path: PathBuf },
    #[error("owner lock refused for {name:?}: {reason:?}")]
    LockRefused { name: OsString, reason: LockRefusal },
    #[error("lock deadline could not be represented for {path:?}")]
    LockDeadlineOverflow { path: PathBuf },
    #[error("stable lock names must be lock or end in .lock: {name:?}")]
    InvalidLockName { name: OsString },
    #[error("stable lock entry cannot be mutated: {name:?}")]
    ReservedLockName { name: OsString },
    #[error("file exceeds byte limit {limit}")]
    TooLarge { limit: u64 },
}
pub type Result<T> = std::result::Result<T, FsError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockRefusal {
    AnchorOwner,
    AnchorPermissions,
    FileOwner,
    FilePermissions,
    LinkCount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}

/// Successful publication status. An unconfirmed directory sync means the
/// new entry is already visible and must not be retried as an absent write.
#[derive(Debug)]
pub enum Publication {
    Durable,
    VisibleDurabilityUnconfirmed { sync_error: FsError },
}

// Fixture consumers exercise the real staging, cleanup and publication path.
mod atomic_write_operations {
    use super::*;
    pub trait AtomicWriteOperations {
        fn write(&mut self, file: &mut FileHandle, bytes: &[u8], path: &Path) -> Result<()>;
        fn sync_file(&mut self, file: &FileHandle) -> Result<()>;
        fn rename(
            &mut self,
            directory: &Directory,
            temporary: &OsStr,
            name: &OsStr,
            mode: PublishMode,
        ) -> Result<()>;
        fn sync_directory(&mut self, directory: &Directory) -> Result<()>;
    }
}
#[cfg(feature = "fixture")]
#[doc(hidden)]
pub use atomic_write_operations::AtomicWriteOperations;
#[cfg(not(feature = "fixture"))]
use atomic_write_operations::AtomicWriteOperations;

struct LocalAtomicWriteOperations;
impl AtomicWriteOperations for LocalAtomicWriteOperations {
    fn write(&mut self, file: &mut FileHandle, bytes: &[u8], path: &Path) -> Result<()> {
        file.write_all(bytes)
            .map_err(error("write staging file", path))
    }
    fn sync_file(&mut self, file: &FileHandle) -> Result<()> {
        file.sync()
    }
    fn rename(
        &mut self,
        directory: &Directory,
        temporary: &OsStr,
        name: &OsStr,
        mode: PublishMode,
    ) -> Result<()> {
        match mode {
            PublishMode::NoReplace => directory.rename_noreplace(temporary, directory, name),
            PublishMode::Replace => directory.replace(temporary, directory, name),
        }
    }
    fn sync_directory(&mut self, directory: &Directory) -> Result<()> {
        directory.sync()
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtomicWritePhase {
    Write,
    FileSync,
    Rename,
    DirectorySync,
}

/// Owning, non-cloneable flock lease. Closing the last owner releases the lock;
/// the API intentionally has no explicit unlock or descriptor duplication.
#[derive(Debug)]
pub struct LockGuard {
    _file: File,
}

impl FsError {
    pub fn raw_os_error(&self) -> Option<i32> {
        match self {
            Self::Io { source, .. }
            | Self::Symlink { source, .. }
            | Self::Rename { source, .. } => source.raw_os_error(),
            Self::CrossDevice { .. } => Some(errno::EXDEV),
            _ => None,
        }
    }
    pub fn is_not_found(&self) -> bool {
        self.raw_os_error() == Some(errno::ENOENT)
    }
    pub fn is_exists(&self) -> bool {
        self.raw_os_error() == Some(errno::EEXIST)
    }
    pub fn is_symlink(&self) -> bool {
        matches!(self, Self::Symlink { .. }) || self.is_loop()
    }
    pub fn is_loop(&self) -> bool {
        self.raw_os_error() == Some(errno::ELOOP)
    }
    pub fn is_cross_device(&self) -> bool {
        self.raw_os_error() == Some(errno::EXDEV)
    }
    pub fn is_not_directory(&self) -> bool {
        self.raw_os_error() == Some(errno::ENOTDIR)
    }
    pub fn is_unsupported(&self) -> bool {
        self.raw_os_error().is_some_and(|code| {
            [
                errno::EINVAL,
                errno::ENOTSUP,
                errno::EOPNOTSUPP,
                errno::ENOSYS,
            ]
            .contains(&code)
        })
    }
}
/// OS failures retain their original io::Error (including errno and kind).
/// Inspect FsError before conversion when operation/path context is needed.
impl From<FsError> for io::Error {
    fn from(error: FsError) -> Self {
        match error {
            FsError::Io { source, .. }
            | FsError::Symlink { source, .. }
            | FsError::Rename { source, .. } => source,
            FsError::CrossDevice { .. } => Self::from_raw_os_error(errno::EXDEV),
            error => Self::new(
                match &error {
                    FsError::InvalidComponent { .. } => io::ErrorKind::InvalidInput,
                    FsError::NotRegular { .. } | FsError::TooLarge { .. } => {
                        io::ErrorKind::InvalidData
                    }
                    _ => io::ErrorKind::Other,
                },
                error,
            ),
        }
    }
}
fn error<'a>(operation: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> FsError + 'a {
    move |source| {
        if source.raw_os_error() == Some(errno::ELOOP) {
            FsError::Symlink {
                path: path.into(),
                source,
            }
        } else {
            FsError::Io {
                operation,
                path: path.into(),
                source,
            }
        }
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

fn is_lock_name(name: &OsStr) -> bool {
    // Case-insensitive filesystems also accept case and Unicode spellings of
    // an existing leaf. Inspect just the extension so non-UTF-8 prefixes retain
    // their native bytes (e.g. Kelvin sign canonically aliases ASCII K).
    let extension = name
        .as_bytes()
        .rsplit(|byte| *byte == b'.')
        .next()
        .unwrap_or_default();
    std::str::from_utf8(extension).is_ok_and(|text| text.to_lowercase() == "lock")
}
fn mutable_component(name: &OsStr) -> Result<&OsStr> {
    component(name)?;
    if is_lock_name(name) {
        return Err(FsError::ReservedLockName { name: name.into() });
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

/// Destination policy for atomic publication. Neither mode follows final links.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishMode {
    NoReplace,
    Replace,
}

/// Owning directory handle. It deliberately implements neither AsRawFd nor Deref.
#[derive(Debug)]
pub struct Directory {
    file: File,
    device: u64,
    // Diagnostic only; never used to resolve an operation. Relative to anchor.
    path: PathBuf,
}
impl Directory {
    /// The initial path is trusted; symlinks in ancestors (e.g. macOS /var) are
    /// permitted. The final component must be a directory, never a symlink.
    pub fn open_anchor(path: &Path) -> Result<Self> {
        Self::from_file(
            os::open_anchor(path).map_err(error("open anchor", path))?,
            PathBuf::new(),
        )
    }
    /// Open / or . without resolving a held directory back to a pathname.
    pub fn root(absolute: bool) -> Result<Self> {
        Self::open_anchor(Path::new(if absolute { "/" } else { "." }))
    }
    fn from_file(file: File, path: PathBuf) -> Result<Self> {
        let device = file
            .metadata()
            .map_err(error("fstat directory", &path))?
            .dev();
        Ok(Self { file, device, path })
    }
    /// Establish this held directory as a new filesystem-device boundary.
    /// Call only after the caller has validated the configured root. Unlike
    /// open_anchor, this consumes the existing handle without resolving a path
    /// again; descendant operations retain the usual no-cross-device policy.
    pub fn into_anchor(self) -> Result<Self> {
        Self::from_file(self.file, self.path)
    }
    pub fn metadata(&self) -> Result<Metadata> {
        self.file
            .metadata()
            .map(Metadata::from_std)
            .map_err(error("fstat directory", &self.path))
    }
    /// Stat a single entry. Following a final link requires explicit authority
    /// from the caller; it does not affect the default no-follow open methods.
    pub fn metadata_at(&self, name: &OsStr, follow: bool) -> Result<Metadata> {
        let name = component(name)?;
        os::metadata_at(&self.file, name, follow).map_err(error("fstatat", &self.path.join(name)))
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
                .map_err(error("openat directory", &self.path.join(name)))?,
            self.path.join(name),
        )?;
        child.device = self.device;
        Ok(child)
    }
    /// Refuse observed identity differences across stat/open/stat and traversal
    /// off the original anchor device. This cannot detect ABA replacement or
    /// prevent entry mutation after the final stat; callers own directory access
    /// policy. The returned descriptor retains its opened identity after rename.
    pub fn open_verified_directory(&self, name: &OsStr) -> Result<Self> {
        let before = self.metadata_at(name, false)?;
        let child = self.open_directory(name)?;
        let opened = child.metadata()?;
        let after = self.metadata_at(name, false)?;
        verify_directory_identity(
            self.path.join(name).as_os_str(),
            before,
            opened,
            after,
            self.device,
        )?;
        Ok(child)
    }
    pub fn open_read(&self, name: &OsStr) -> Result<FileHandle> {
        let name = component(name)?;
        FileHandle::regular(
            os::open_read(&self.file, name).map_err(error("openat read", &self.path.join(name)))?,
            &self.path.join(name),
        )
    }
    /// Open and validate a stable owner lock, then acquire it with bounded
    /// nonblocking polling. The supplied clock interprets the WaitBudget's
    /// process-local monotonic coordinate; its current sample is taken once.
    pub fn lock(
        &self,
        name: &OsStr,
        mode: LockMode,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<LockGuard> {
        let name = component(name)?;
        if name != "lock" && !name.as_bytes().ends_with(b".lock") {
            return Err(FsError::InvalidLockName { name: name.into() });
        }
        let path = self.path.join(name);
        if cancel.is_cancelled() {
            return Err(FsError::Cancelled { path });
        }
        let remaining = budget.remaining(clock.mono_now());
        let timeout = Duration::from_micros(remaining.as_micros());
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| FsError::LockDeadlineOverflow { path: path.clone() })?;
        let owner_uid = os::effective_uid();
        let directory_metadata = self.metadata()?;
        verify_lock_directory(name, directory_metadata, owner_uid)?;
        let before = match self.metadata_at(name, false) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.is_not_found() => None,
            Err(error) => return Err(error),
        };
        let file = os::open_lock(&self.file, name).map_err(error("open owner lock", &path))?;
        let opened = file
            .metadata()
            .map(Metadata::from_std)
            .map_err(error("fstat owner lock", &path))?;
        let after = self.metadata_at(name, false)?;
        verify_owner_lock_metadata(name, before, opened, after, self.device, owner_uid)?;

        loop {
            match os::try_flock(&file, mode) {
                Ok(()) => {
                    if cancel.is_cancelled() {
                        return Err(FsError::Cancelled { path });
                    }
                    return Ok(LockGuard { _file: file });
                }
                Err(source)
                    if source.kind() == io::ErrorKind::WouldBlock
                        || source.raw_os_error() == Some(errno::EINTR) => {}
                Err(source) => return Err(error("flock", &path)(source)),
            }
            if cancel.is_cancelled() {
                return Err(FsError::Cancelled { path });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(FsError::LockTimeout { path, timeout });
            }
            os::wait(remaining.min(Duration::from_millis(5)));
        }
    }
    /// Exclusive create, mode 0600 subject to umask, O_NOFOLLOW | O_CLOEXEC.
    pub fn create_new(&self, name: &OsStr) -> Result<FileHandle> {
        let name = component(name)?;
        FileHandle::regular(
            os::create_new(&self.file, name)
                .map_err(error("openat create", &self.path.join(name)))?,
            &self.path.join(name),
        )
    }
    pub fn mkdir(&self, name: &OsStr, mode: u32) -> Result<()> {
        let name = component(name)?;
        os::mkdir(&self.file, name, mode).map_err(error("mkdirat", &self.path.join(name)))
    }
    pub fn unlink(&self, name: &OsStr) -> Result<()> {
        self.remove(name, false)
    }
    pub fn rmdir(&self, name: &OsStr) -> Result<()> {
        self.remove(name, true)
    }
    fn remove(&self, name: &OsStr, directory: bool) -> Result<()> {
        let name = mutable_component(name)?;
        os::unlink(&self.file, name, directory).map_err(error("unlinkat", &self.path.join(name)))
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
    // Kept crate-private until an authorised caller lands.
    #[allow(dead_code)]
    pub(crate) fn exchange(
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
        mutable_component(source)?;
        mutable_component(destination)?;
        os::rename(&self.file, source, &destination_dir.file, destination, mode).map_err(
            |source_error| FsError::Rename {
                from_path: self.path.join(source),
                to_path: destination_dir.path.join(destination),
                source: source_error,
            },
        )
    }
    pub fn read_link(&self, name: &OsStr) -> Result<PathBuf> {
        component(name)?;
        os::read_link(&self.file, name).map_err(error("readlinkat", &self.path.join(name)))
    }
    /// Stage exact bytes and permissions before publication; the destination is
    /// never opened or chmod'd. All effects use the held directory capability.
    pub fn publish(
        &self,
        name: &OsStr,
        bytes: &[u8],
        permissions: u32,
        mode: PublishMode,
    ) -> Result<Publication> {
        mutable_component(name)?;
        let temporary = temporary_name()?;
        // Creation failure must never trigger cleanup of somebody else's entry.
        let mut file = self.create_new(&temporary)?;
        let result = (|| {
            file.write_all(bytes)
                .map_err(error("write staging file", &self.path.join(&temporary)))?;
            file.file
                .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(permissions))
                .map_err(error("fchmod staging file", &self.path.join(&temporary)))?;
            file.sync()?;
            match mode {
                PublishMode::Replace => self.replace(&temporary, self, name),
                PublishMode::NoReplace => self.rename_noreplace(&temporary, self, name),
            }
        })();
        self.finish_publication(&temporary, result, || self.sync())
    }
    pub fn symlink(&self, target: &Path, name: &OsStr) -> Result<()> {
        let name = mutable_component(name)?;
        os::symlink(target, &self.file, name).map_err(error("symlinkat", &self.path.join(name)))
    }
    pub fn sync(&self) -> Result<()> {
        self.file
            .sync_all()
            .map_err(error("fsync directory", &self.path))
    }
    pub fn make_writable(&self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let permissions = self
            .file
            .metadata()
            .map_err(error("fstat directory", &self.path))?
            .permissions();
        self.file
            .set_permissions(std::fs::Permissions::from_mode(permissions.mode() | 0o700))
            .map_err(error("fchmod directory", &self.path))
    }
    /// A fresh descriptor stream on each call, independent of prior enumeration.
    pub fn entries(&self) -> Result<Entries> {
        os::entries(&self.file)
            .map(|stream| Entries {
                stream,
                path: self.path.clone(),
            })
            .map_err(error("opendir", &self.path))
    }
    /// Stage mode-0600 bytes in this directory, fsync the file, atomically
    /// publish, then fsync the directory. A post-rename sync error means the
    /// entry is visible but durability is unconfirmed. The directory must be
    /// writable only by trusted actors. Failed staging is cleaned up best-effort;
    /// a cleanup failure never replaces the original error.
    pub fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        mode: PublishMode,
    ) -> Result<Publication> {
        let mut operations = LocalAtomicWriteOperations;
        self.atomic_write_with_operations(name, prefix, bytes, mode, &mut operations)
    }
    /// Fixture entry for other crates' fault tests; the trait is public only
    /// under `fixture`, so this cannot take the private method's place.
    #[cfg(feature = "fixture")]
    #[doc(hidden)]
    pub fn atomic_write_with_faults(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        mode: PublishMode,
        faults: &mut dyn AtomicWriteOperations,
    ) -> Result<Publication> {
        self.atomic_write_with_operations(name, prefix, bytes, mode, faults)
    }
    fn atomic_write_with_operations(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        mode: PublishMode,
        operations: &mut dyn AtomicWriteOperations,
    ) -> Result<Publication> {
        component(name)?;
        component(OsStr::new(prefix))?;
        for _ in 0..128 {
            let temporary = staging_name(prefix)?;
            let mut file = match self.create_new(&temporary) {
                Ok(file) => file,
                Err(error) if error.is_exists() => continue,
                Err(error) => return Err(error),
            };
            let temporary_path = self.path.join(&temporary);
            let result = operations
                .write(&mut file, bytes, &temporary_path)
                .and_then(|()| operations.sync_file(&file));
            drop(file);
            let result = result.and_then(|()| operations.rename(self, &temporary, name, mode));
            return self.finish_publication(&temporary, result, || operations.sync_directory(self));
        }
        Err(error("reserve staging file", &self.path.join(name))(
            io::Error::from_raw_os_error(errno::EEXIST),
        ))
    }
    /// Atomically replace an entry with a staged symlink and fsync the directory.
    /// Target bytes are stored verbatim; this operation does not follow them.
    /// The same trusted-directory and post-rename durability rules apply.
    pub fn replace_link(&self, name: &OsStr, target: &Path) -> Result<Publication> {
        self.replace_link_with_sync(name, target, || self.sync())
    }
    fn replace_link_with_sync(
        &self,
        name: &OsStr,
        target: &Path,
        sync_directory: impl FnOnce() -> Result<()>,
    ) -> Result<Publication> {
        component(name)?;
        for _ in 0..128 {
            let temporary = staging_name(".provenant-staging.")?;
            match self.symlink(target, &temporary) {
                Ok(()) => {}
                Err(error) if error.is_exists() => continue,
                Err(error) => return Err(error),
            }
            return self.finish_publication(
                &temporary,
                self.replace(&temporary, self, name),
                sync_directory,
            );
        }
        Err(error("reserve staging link", &self.path.join(name))(
            io::Error::from_raw_os_error(errno::EEXIST),
        ))
    }
    fn finish_publication(
        &self,
        temporary: &OsStr,
        result: Result<()>,
        sync_directory: impl FnOnce() -> Result<()>,
    ) -> Result<Publication> {
        if let Err(error) = result {
            let _ = self.unlink(temporary);
            return Err(error);
        }
        // A successful rename already consumed temporary. Never unlink its name
        // after publication: a subsequent creator could now own that entry.
        match sync_directory() {
            Ok(()) => Ok(Publication::Durable),
            Err(sync_error) => Ok(Publication::VisibleDurabilityUnconfirmed { sync_error }),
        }
    }
    pub fn read_bounded(&self, name: &OsStr, limit: u64) -> Result<Vec<u8>> {
        self.open_read(name)?.read_bounded(limit)
    }
}

fn verify_lock_directory(name: &OsStr, directory: Metadata, owner_uid: u32) -> Result<()> {
    if directory.kind != FileKind::Directory {
        return Err(FsError::RaceRefused { name: name.into() });
    }
    if directory.uid != owner_uid {
        return Err(FsError::LockRefused {
            name: name.into(),
            reason: LockRefusal::AnchorOwner,
        });
    }
    if directory.mode & 0o022 != 0 {
        return Err(FsError::LockRefused {
            name: name.into(),
            reason: LockRefusal::AnchorPermissions,
        });
    }
    Ok(())
}

fn verify_owner_lock_metadata(
    name: &OsStr,
    before: Option<Metadata>,
    opened: Metadata,
    after: Metadata,
    anchor_device: u64,
    owner_uid: u32,
) -> Result<()> {
    if opened.device != anchor_device
        || after.device != anchor_device
        || before.is_some_and(|metadata| metadata.device != anchor_device)
    {
        return Err(FsError::CrossDevice { path: name.into() });
    }
    if opened.kind != FileKind::Regular
        || after.kind != FileKind::Regular
        || before.is_some_and(|metadata| metadata.kind != FileKind::Regular)
    {
        return Err(FsError::NotRegular { name: name.into() });
    }
    if !opened.same_identity(&after)
        || before.is_some_and(|metadata| !metadata.same_identity(&opened))
    {
        return Err(FsError::RaceRefused { name: name.into() });
    }
    if opened.links != 1 || after.links != 1 || before.is_some_and(|metadata| metadata.links != 1) {
        return Err(FsError::LockRefused {
            name: name.into(),
            reason: LockRefusal::LinkCount,
        });
    }
    if opened.uid != owner_uid
        || after.uid != owner_uid
        || before.is_some_and(|metadata| metadata.uid != owner_uid)
    {
        return Err(FsError::LockRefused {
            name: name.into(),
            reason: LockRefusal::FileOwner,
        });
    }
    if opened.mode & 0o022 != 0
        || after.mode & 0o022 != 0
        || before.is_some_and(|metadata| metadata.mode & 0o022 != 0)
    {
        return Err(FsError::LockRefused {
            name: name.into(),
            reason: LockRefusal::FilePermissions,
        });
    }
    Ok(())
}

/// Pure refusal policy over stat/open/stat observations. This grants no handle
/// authority: the OS adapter must retain the opened descriptor through use.
pub fn verify_directory_identity(
    name: &OsStr,
    before: Metadata,
    opened: Metadata,
    after: Metadata,
    anchor_device: u64,
) -> Result<()> {
    if opened.device != anchor_device {
        return Err(FsError::CrossDevice { path: name.into() });
    }
    if before.kind != FileKind::Directory
        || opened.kind != FileKind::Directory
        || after.kind != FileKind::Directory
        || !before.same_identity(&opened)
        || !opened.same_identity(&after)
    {
        return Err(FsError::RaceRefused { name: name.into() });
    }
    Ok(())
}

/// Lazy, owning directory stream. Each item retains the original OS error and
/// its directory context. Dropping the anchor does not close this stream's
/// independent descriptor.
pub struct Entries {
    stream: os::Entries,
    path: PathBuf,
}
impl Iterator for Entries {
    type Item = Result<OsString>;
    fn next(&mut self) -> Option<Self::Item> {
        self.stream
            .next()
            .map(|result| result.map_err(error("readdir", &self.path)))
    }
}

/// File capability; no raw owning descriptor escapes the supervisor boundary.
#[derive(Debug)]
pub struct FileHandle {
    file: File,
    path: PathBuf,
}
impl FileHandle {
    fn regular(file: File, path: &Path) -> Result<Self> {
        if !file
            .metadata()
            .map_err(error("fstat file", path))?
            .is_file()
        {
            return Err(FsError::NotRegular {
                name: path.as_os_str().into(),
            });
        }
        os::clear_nonblocking(&file).map_err(error("clear nonblocking", path))?;
        Ok(Self {
            file,
            path: path.into(),
        })
    }
    /// Trusted initial source path; O_NOFOLLOW applies to its final component.
    pub fn open_source(path: &Path) -> Result<Self> {
        Self::regular(
            os::open_source(path).map_err(error("open source", path))?,
            path,
        )
    }
    /// Like dup: shared file offset; descriptor remains close-on-exec.
    pub fn duplicate(&self) -> Result<Self> {
        self.file
            .try_clone()
            .map(|file| Self {
                file,
                path: self.path.clone(),
            })
            .map_err(error("duplicate file", &self.path))
    }
    pub fn metadata(&self) -> Result<Metadata> {
        self.file
            .metadata()
            .map(Metadata::from_std)
            .map_err(error("fstat file", &self.path))
    }
    pub fn sync(&self) -> Result<()> {
        self.file
            .sync_all()
            .map_err(error("fsync file", &self.path))
    }
    /// Read at most limit+1 bytes from the current offset, then refuse oversize.
    /// Metadata alone is never used to enforce the cap (files can grow).
    pub fn read_bounded(&mut self, limit: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        (&mut self.file)
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(error("read file", &self.path))?;
        if bytes.len() as u64 > limit {
            return Err(FsError::TooLarge { limit });
        }
        Ok(bytes)
    }
}
impl Read for FileHandle {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.file.read(b)
    }
}
impl Write for FileHandle {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.file.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for FileHandle {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        self.file.seek(p)
    }
}

/// OS entropy, no global counter or clock; callers use exclusive creation too.
pub fn temporary_name() -> Result<OsString> {
    let bytes = os::entropy().map_err(error("getentropy", Path::new("<temporary name>")))?;
    let mut name = String::from(".provenant-staging.");
    for byte in bytes {
        name.push_str(&format!("{byte:02x}"));
    }
    Ok(name.into())
}

#[cfg(test)]
mod tests;

fn staging_name(prefix: &str) -> Result<OsString> {
    let random = temporary_name()?;
    let mut name = OsString::from(prefix);
    name.push(random);
    name.push(".tmp");
    component(&name)?;
    Ok(name)
}

mod traits;
pub use traits::{LocalFs, SafeDir, SafeFile, SafeFs};

mod path_io;
pub use path_io::*;
