//! Object-safe filesystem seam. Handles retain confinement authority; adapters
//! translate metadata records, never descriptors or held paths. Filesystem
//! methods keep typed errors across trait objects; file stream operations use
//! the standard `io::Result` inherited from `Read`, `Write`, and `Seek`.
use super::*;
use provenant_core::{CancelToken, Clock, WaitBudget};

pub trait SafeFile: Read + Write + Seek + Send + Sync {
    fn metadata(&self) -> Result<Metadata>;
    /// Retains the shared offset, like dup (not a reopen).
    fn duplicate(&self) -> Result<Box<dyn SafeFile>>;
    fn sync(&self) -> Result<()>;
}

/// Names are exactly one normal component. Implementations must use held
/// directory authority, preserve contextual `FsError` values, and never follow
/// final links unless explicitly requested. Same-directory publication keeps
/// this trait object-safe; concrete Directory also supports publication between
/// held directories.
pub trait SafeDir: Send + Sync {
    /// Establish a validated, held no-follow directory as a device boundary.
    /// Adapters without this capability refuse; no path is resolved again.
    fn into_anchor(self: Box<Self>) -> Result<Box<dyn SafeDir>> {
        Err(FsError::Io {
            operation: "held anchor unsupported",
            path: PathBuf::new(),
            source: io::Error::from(io::ErrorKind::Unsupported),
        })
    }
    fn metadata(&self) -> Result<Metadata>;
    fn metadata_at(&self, name: &OsStr, follow: bool) -> Result<Metadata>;
    fn open_directory(&self, name: &OsStr, follow: bool) -> Result<Box<dyn SafeDir>>;
    fn open_verified_directory(&self, name: &OsStr) -> Result<Box<dyn SafeDir>>;
    fn open_read(&self, name: &OsStr) -> Result<Box<dyn SafeFile>>;
    fn create_new(&self, name: &OsStr) -> Result<Box<dyn SafeFile>>;
    fn mkdir(&self, name: &OsStr, mode: u32) -> Result<()>;
    fn entries(&self) -> Result<Box<dyn Iterator<Item = Result<OsString>>>>;
    fn make_writable(&self) -> Result<()>;
    /// Observe an existing stable lock without creating or repairing it.
    /// Adapters without this capability refuse observation.
    fn try_lock_existing(&self, name: &OsStr, mode: LockMode) -> Result<LockGuard> {
        let _ = mode;
        Err(FsError::Io {
            operation: "existing lock unsupported",
            path: name.into(),
            source: io::Error::from(io::ErrorKind::Unsupported),
        })
    }
    fn lock(
        &self,
        name: &OsStr,
        mode: LockMode,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<LockGuard>;
    fn unlink(&self, name: &OsStr) -> Result<()>;
    fn rmdir(&self, name: &OsStr) -> Result<()>;
    /// Payload removal, reachable only through `PayloadTree`, which alone can
    /// grant it. Wrapping adapters forward the grant; others retain ordinary
    /// lock protection unless they explicitly support payload removal.
    fn remove_payload(&self, name: &OsStr, directory: bool, grant: PayloadRemoval) -> Result<()> {
        let _ = grant;
        if directory {
            self.rmdir(name)
        } else {
            self.unlink(name)
        }
    }
    fn replace(&self, source: &OsStr, destination: &OsStr) -> Result<()>;
    fn rename_noreplace(&self, source: &OsStr, destination: &OsStr) -> Result<()>;
    /// Atomically move an entry between held directories without replacement.
    /// Unsupported adapters refuse; never copy/unlink or cross-device fallback.
    fn rename_noreplace_to(
        &self,
        source: &OsStr,
        destination_dir: &dyn SafeDir,
        destination: &OsStr,
    ) -> Result<()> {
        let _ = (destination_dir, destination);
        Err(FsError::Io {
            operation: "cross-directory rename unsupported",
            path: source.into(),
            source: io::Error::from(io::ErrorKind::Unsupported),
        })
    }
    /// Safe double-dispatch for a native source capability. No descriptor escapes.
    #[doc(hidden)]
    fn rename_noreplace_from(
        &self,
        source_dir: &Directory,
        source: &OsStr,
        destination: &OsStr,
    ) -> Result<()> {
        let _ = (source_dir, destination);
        Err(FsError::Io {
            operation: "cross-directory rename unsupported",
            path: source.into(),
            source: io::Error::from(io::ErrorKind::Unsupported),
        })
    }

    fn symlink(&self, target: &Path, name: &OsStr) -> Result<()>;
    fn read_link(&self, name: &OsStr) -> Result<PathBuf>;
    fn sync(&self) -> Result<()>;
    fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        permissions: u32,
        mode: PublishMode,
    ) -> Result<Publication>;
    fn replace_link(&self, name: &OsStr, target: &Path) -> Result<Publication>;
}

/// Initial trusted-path access and entropy are injectable too, so consumer
/// adapters can exercise their complete policy without a live filesystem.
pub trait SafeFs: Send + Sync {
    fn root(&self, absolute: bool) -> Result<Box<dyn SafeDir>>;
    fn open_anchor(&self, path: &Path) -> Result<Box<dyn SafeDir>>;
    fn open_source(&self, path: &Path) -> Result<Box<dyn SafeFile>>;
    fn temporary_name(&self) -> Result<OsString>;
    fn entropy_128(&self) -> Result<[u8; 16]> {
        super::entropy_128()
    }
}

/// Snapshot one directory before mutating its entries. A lazy readdir stream
/// may skip names when entries are removed between reads (notably on APFS).
/// Refuse oversized directories before the caller starts deleting anything.
pub fn entry_names(directory: &dyn SafeDir, limit: usize) -> Result<Vec<OsString>> {
    let mut names = Vec::new();
    for entry in directory.entries()? {
        let name = entry?;
        if names.len() == limit {
            return Err(FsError::EntryLimit { limit });
        }
        names.push(name);
    }
    Ok(names)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalFs;
impl SafeFs for LocalFs {
    fn root(&self, absolute: bool) -> Result<Box<dyn SafeDir>> {
        Ok(Box::new(Directory::root(absolute)?))
    }
    fn open_anchor(&self, path: &Path) -> Result<Box<dyn SafeDir>> {
        Ok(Box::new(Directory::open_anchor(path)?))
    }
    fn open_source(&self, path: &Path) -> Result<Box<dyn SafeFile>> {
        Ok(Box::new(FileHandle::open_source(path)?))
    }
    fn temporary_name(&self) -> Result<OsString> {
        temporary_name()
    }
}

struct PermissionedAtomicWriteOperations {
    inner: LocalAtomicWriteOperations,
    permissions: u32,
}
impl AtomicWriteOperations for PermissionedAtomicWriteOperations {
    fn write(&mut self, file: &mut FileHandle, bytes: &[u8], path: &Path) -> Result<()> {
        self.inner.write(file, bytes, path)?;
        use std::os::unix::fs::PermissionsExt;
        file.file
            .set_permissions(std::fs::Permissions::from_mode(self.permissions))
            .map_err(error("fchmod staging file", path))
    }
    fn sync_file(&mut self, file: &FileHandle) -> Result<()> {
        self.inner.sync_file(file)
    }
    fn rename(
        &mut self,
        directory: &Directory,
        temporary: &OsStr,
        name: &OsStr,
        mode: PublishMode,
    ) -> Result<()> {
        self.inner.rename(directory, temporary, name, mode)
    }
    fn sync_directory(&mut self, directory: &Directory) -> Result<()> {
        self.inner.sync_directory(directory)
    }
}

impl SafeFile for FileHandle {
    fn metadata(&self) -> Result<Metadata> {
        FileHandle::metadata(self)
    }
    fn duplicate(&self) -> Result<Box<dyn SafeFile>> {
        Ok(Box::new(FileHandle::duplicate(self)?))
    }
    fn sync(&self) -> Result<()> {
        FileHandle::sync(self)
    }
}
impl SafeDir for Directory {
    fn try_lock_existing(&self, name: &OsStr, mode: LockMode) -> Result<LockGuard> {
        Directory::try_lock_existing(self, name, mode)
    }
    /// Unlike `unlink`, this permits payload names such as Cargo.lock. No
    /// final symlink is followed.
    fn remove_payload(&self, name: &OsStr, directory: bool, _: PayloadRemoval) -> Result<()> {
        let name = component(name)?;
        let operation = if directory {
            "rmdir payload"
        } else {
            "unlinkat payload"
        };
        os::unlink(&self.file, name, directory).map_err(error(operation, &self.path.join(name)))
    }
    fn into_anchor(self: Box<Self>) -> Result<Box<dyn SafeDir>> {
        Ok(Box::new(Directory::into_anchor(*self)?))
    }
    fn metadata(&self) -> Result<Metadata> {
        Directory::metadata(self)
    }
    fn metadata_at(&self, name: &OsStr, follow: bool) -> Result<Metadata> {
        Directory::metadata_at(self, name, follow)
    }
    fn open_directory(&self, name: &OsStr, follow: bool) -> Result<Box<dyn SafeDir>> {
        Ok(Box::new(self.open_directory_with_links(name, follow)?))
    }
    fn open_verified_directory(&self, name: &OsStr) -> Result<Box<dyn SafeDir>> {
        Ok(Box::new(Directory::open_verified_directory(self, name)?))
    }
    fn open_read(&self, name: &OsStr) -> Result<Box<dyn SafeFile>> {
        Ok(Box::new(Directory::open_read(self, name)?))
    }
    fn create_new(&self, name: &OsStr) -> Result<Box<dyn SafeFile>> {
        Ok(Box::new(Directory::create_new(self, name)?))
    }
    fn mkdir(&self, name: &OsStr, mode: u32) -> Result<()> {
        Directory::mkdir(self, name, mode)
    }
    fn entries(&self) -> Result<Box<dyn Iterator<Item = Result<OsString>>>> {
        Ok(Box::new(Directory::entries(self)?))
    }
    fn make_writable(&self) -> Result<()> {
        Directory::make_writable(self)
    }
    fn lock(
        &self,
        name: &OsStr,
        mode: LockMode,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<LockGuard> {
        Directory::lock(self, name, mode, budget, cancel, clock)
    }
    fn unlink(&self, name: &OsStr) -> Result<()> {
        Directory::unlink(self, name)
    }
    fn rmdir(&self, name: &OsStr) -> Result<()> {
        Directory::rmdir(self, name)
    }
    fn replace(&self, source: &OsStr, destination: &OsStr) -> Result<()> {
        Directory::replace(self, source, self, destination)
    }
    fn rename_noreplace(&self, source: &OsStr, destination: &OsStr) -> Result<()> {
        Directory::rename_noreplace(self, source, self, destination)
    }
    fn rename_noreplace_to(
        &self,
        source: &OsStr,
        destination_dir: &dyn SafeDir,
        destination: &OsStr,
    ) -> Result<()> {
        destination_dir.rename_noreplace_from(self, source, destination)
    }
    fn rename_noreplace_from(
        &self,
        source_dir: &Directory,
        source: &OsStr,
        destination: &OsStr,
    ) -> Result<()> {
        Directory::rename_noreplace(source_dir, source, self, destination)
    }
    fn symlink(&self, target: &Path, name: &OsStr) -> Result<()> {
        Directory::symlink(self, target, name)
    }
    fn read_link(&self, name: &OsStr) -> Result<PathBuf> {
        let name = component(name)?;
        os::read_link(&self.file, name).map_err(error("readlinkat", &self.path.join(name)))
    }
    fn sync(&self) -> Result<()> {
        Directory::sync(self)
    }
    fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        permissions: u32,
        mode: PublishMode,
    ) -> Result<Publication> {
        let mut operations = PermissionedAtomicWriteOperations {
            inner: LocalAtomicWriteOperations,
            permissions,
        };
        Directory::atomic_write_with_operations(self, name, prefix, bytes, mode, &mut operations)
    }
    fn replace_link(&self, name: &OsStr, target: &Path) -> Result<Publication> {
        Directory::replace_link(self, name, target)
    }
}
