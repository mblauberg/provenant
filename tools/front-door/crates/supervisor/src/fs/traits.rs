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
    fn metadata(&self) -> Result<Metadata>;
    fn metadata_at(&self, name: &OsStr, follow: bool) -> Result<Metadata>;
    fn open_directory(&self, name: &OsStr, follow: bool) -> Result<Box<dyn SafeDir>>;
    fn open_verified_directory(&self, name: &OsStr) -> Result<Box<dyn SafeDir>>;
    fn open_read(&self, name: &OsStr) -> Result<Box<dyn SafeFile>>;
    fn create_new(&self, name: &OsStr) -> Result<Box<dyn SafeFile>>;
    fn mkdir(&self, name: &OsStr, mode: u32) -> Result<()>;
    fn entries(&self) -> Result<Box<dyn Iterator<Item = Result<OsString>>>>;
    fn make_writable(&self) -> Result<()>;
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
    fn sync(&self) -> Result<()>;
    fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
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
    fn sync(&self) -> Result<()> {
        Directory::sync(self)
    }
    fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        mode: PublishMode,
    ) -> Result<Publication> {
        Directory::atomic_write(self, name, prefix, bytes, mode)
    }
    fn replace_link(&self, name: &OsStr, target: &Path) -> Result<Publication> {
        Directory::replace_link(self, name, target)
    }
}
