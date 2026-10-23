//! Object-safe filesystem seam. Handles retain confinement authority; adapters
//! translate metadata records, never descriptors or held paths.
use super::*;
use provenant_core::{CancelToken, Clock, WaitBudget};

pub trait SafeFile: Read + Write + Seek + Send + Sync {
    fn metadata(&self) -> io::Result<Metadata>;
    /// Retains the shared offset, like dup (not a reopen).
    fn duplicate(&self) -> io::Result<Box<dyn SafeFile>>;
    fn sync(&self) -> io::Result<()>;
}

/// Names are exactly one normal component. Implementations must use held
/// directory authority, preserve OS errors, and never follow final links unless
/// explicitly requested. Same-directory publication keeps this trait object-safe;
/// concrete Directory also supports publication between held directories.
pub trait SafeDir: Send + Sync {
    fn metadata(&self) -> io::Result<Metadata>;
    fn metadata_at(&self, name: &OsStr, follow: bool) -> io::Result<Metadata>;
    fn open_directory(&self, name: &OsStr, follow: bool) -> io::Result<Box<dyn SafeDir>>;
    fn open_verified_directory(&self, name: &OsStr) -> io::Result<Box<dyn SafeDir>>;
    fn open_read(&self, name: &OsStr) -> io::Result<Box<dyn SafeFile>>;
    fn create_new(&self, name: &OsStr) -> io::Result<Box<dyn SafeFile>>;
    fn mkdir(&self, name: &OsStr, mode: u32) -> io::Result<()>;
    fn entries(&self) -> io::Result<Box<dyn Iterator<Item = io::Result<OsString>>>>;
    fn make_writable(&self) -> io::Result<()>;
    fn lock(
        &self,
        name: &OsStr,
        mode: LockMode,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> io::Result<LockGuard>;
    fn unlink(&self, name: &OsStr) -> io::Result<()>;
    fn rmdir(&self, name: &OsStr) -> io::Result<()>;
    fn replace(&self, source: &OsStr, destination: &OsStr) -> io::Result<()>;
    fn rename_noreplace(&self, source: &OsStr, destination: &OsStr) -> io::Result<()>;
    fn exchange(&self, source: &OsStr, destination: &OsStr) -> io::Result<()>;
    fn symlink(&self, target: &Path, name: &OsStr) -> io::Result<()>;
    fn sync(&self) -> io::Result<()>;
    fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        mode: PublishMode,
    ) -> io::Result<Publication>;
    fn replace_link(&self, name: &OsStr, target: &Path) -> io::Result<Publication>;
}

/// Initial trusted-path access and entropy are injectable too, so consumer
/// adapters can exercise their complete policy without a live filesystem.
pub trait SafeFs: Send + Sync {
    fn root(&self, absolute: bool) -> io::Result<Box<dyn SafeDir>>;
    fn open_anchor(&self, path: &Path) -> io::Result<Box<dyn SafeDir>>;
    fn open_source(&self, path: &Path) -> io::Result<Box<dyn SafeFile>>;
    fn temporary_name(&self) -> io::Result<OsString>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LocalFs;
impl SafeFs for LocalFs {
    fn root(&self, absolute: bool) -> io::Result<Box<dyn SafeDir>> {
        Ok(Box::new(Directory::root(absolute)?))
    }
    fn open_anchor(&self, path: &Path) -> io::Result<Box<dyn SafeDir>> {
        Ok(Box::new(Directory::open_anchor(path)?))
    }
    fn open_source(&self, path: &Path) -> io::Result<Box<dyn SafeFile>> {
        Ok(Box::new(FileHandle::open_source(path)?))
    }
    fn temporary_name(&self) -> io::Result<OsString> {
        temporary_name().map_err(Into::into)
    }
}

impl SafeFile for FileHandle {
    fn metadata(&self) -> io::Result<Metadata> {
        FileHandle::metadata(self).map_err(Into::into)
    }
    fn duplicate(&self) -> io::Result<Box<dyn SafeFile>> {
        Ok(Box::new(FileHandle::duplicate(self)?))
    }
    fn sync(&self) -> io::Result<()> {
        FileHandle::sync(self).map_err(Into::into)
    }
}
impl SafeDir for Directory {
    fn metadata(&self) -> io::Result<Metadata> {
        Directory::metadata(self).map_err(Into::into)
    }
    fn metadata_at(&self, name: &OsStr, follow: bool) -> io::Result<Metadata> {
        Directory::metadata_at(self, name, follow).map_err(Into::into)
    }
    fn open_directory(&self, name: &OsStr, follow: bool) -> io::Result<Box<dyn SafeDir>> {
        Ok(Box::new(self.open_directory_with_links(name, follow)?))
    }
    fn open_verified_directory(&self, name: &OsStr) -> io::Result<Box<dyn SafeDir>> {
        Ok(Box::new(Directory::open_verified_directory(self, name)?))
    }
    fn open_read(&self, name: &OsStr) -> io::Result<Box<dyn SafeFile>> {
        Ok(Box::new(Directory::open_read(self, name)?))
    }
    fn create_new(&self, name: &OsStr) -> io::Result<Box<dyn SafeFile>> {
        Ok(Box::new(Directory::create_new(self, name)?))
    }
    fn mkdir(&self, name: &OsStr, mode: u32) -> io::Result<()> {
        Directory::mkdir(self, name, mode).map_err(Into::into)
    }
    fn entries(&self) -> io::Result<Box<dyn Iterator<Item = io::Result<OsString>>>> {
        Ok(Box::new(Directory::entries(self)?))
    }
    fn make_writable(&self) -> io::Result<()> {
        Directory::make_writable(self).map_err(Into::into)
    }
    fn lock(
        &self,
        name: &OsStr,
        mode: LockMode,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> io::Result<LockGuard> {
        Directory::lock(self, name, mode, budget, cancel, clock).map_err(Into::into)
    }
    fn unlink(&self, name: &OsStr) -> io::Result<()> {
        Directory::unlink(self, name).map_err(Into::into)
    }
    fn rmdir(&self, name: &OsStr) -> io::Result<()> {
        Directory::rmdir(self, name).map_err(Into::into)
    }
    fn replace(&self, source: &OsStr, destination: &OsStr) -> io::Result<()> {
        Directory::replace(self, source, self, destination).map_err(Into::into)
    }
    fn rename_noreplace(&self, source: &OsStr, destination: &OsStr) -> io::Result<()> {
        Directory::rename_noreplace(self, source, self, destination).map_err(Into::into)
    }
    fn exchange(&self, source: &OsStr, destination: &OsStr) -> io::Result<()> {
        Directory::exchange(self, source, self, destination).map_err(Into::into)
    }
    fn symlink(&self, target: &Path, name: &OsStr) -> io::Result<()> {
        Directory::symlink(self, target, name).map_err(Into::into)
    }
    fn sync(&self) -> io::Result<()> {
        Directory::sync(self).map_err(Into::into)
    }
    fn atomic_write(
        &self,
        name: &OsStr,
        prefix: &str,
        bytes: &[u8],
        mode: PublishMode,
    ) -> io::Result<Publication> {
        Directory::atomic_write(self, name, prefix, bytes, mode).map_err(Into::into)
    }
    fn replace_link(&self, name: &OsStr, target: &Path) -> io::Result<Publication> {
        Directory::replace_link(self, name, target).map_err(Into::into)
    }
}
