//! Safe filesystem contract awaiting the supervisor syscall owner. No backend is shipped here.
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Seek, Write};
use std::path::Path;

// port: belongs in provenant-supervisor
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind { Regular, Directory, Symlink, Other }

// port: belongs in provenant-supervisor
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Metadata {
    pub kind: FileKind,
    pub device: u64,
    pub inode: u64,
    pub links: u64,
    pub size: u64,
    pub modified_ns: i128,
    pub uid: u32,
    pub mode: u32,
}

// port: belongs in provenant-supervisor
/// Owning handles close on Drop. All relative operations are descriptor-relative;
/// names are single components. No handle exposes a raw descriptor or signalling API.
/// Implementations must preserve OS errors, and must not resolve a held directory by path.
pub trait CustodyFs {
    type Directory;
    type File: Read + Write + Seek;
    fn root(&self, absolute: bool) -> io::Result<Self::Directory>;
    fn directory_metadata(&self, dir: &Self::Directory) -> io::Result<Metadata>;
    fn metadata_at(&self, dir: &Self::Directory, name: &OsStr, follow: bool) -> io::Result<Metadata>;
    /// O_DIRECTORY; O_NOFOLLOW unless follow is explicitly permitted by the caller.
    fn open_directory(&self, dir: &Self::Directory, name: &OsStr, follow: bool) -> io::Result<Self::Directory>;
    /// O_RDONLY | O_NOFOLLOW, without creation.
    fn open_read(&self, dir: &Self::Directory, name: &OsStr) -> io::Result<Self::File>;
    /// O_RDONLY | O_NOFOLLOW, without creation.
    fn open_source(&self, path: &Path) -> io::Result<Self::File>;
    /// Duplicate an already-held source; the shared offset is deliberately retained.
    fn duplicate(&self, file: &Self::File) -> io::Result<Self::File>;
    fn file_metadata(&self, file: &Self::File) -> io::Result<Metadata>;
    /// Fresh .cf-dispatch-output.<24 hex digits>, from the OS entropy source.
    fn temporary_name(&self) -> io::Result<OsString>;
    /// O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, mode 0600 (subject to umask).
    fn create_new(&self, dir: &Self::Directory, name: &OsStr) -> io::Result<Self::File>;
    fn sync_file(&self, file: &Self::File) -> io::Result<()>;
    fn sync_directory(&self, dir: &Self::Directory) -> io::Result<()>;
    fn replace(&self, dir: &Self::Directory, source: &OsStr, destination: &OsStr) -> io::Result<()>;
    fn unlink(&self, dir: &Self::Directory, name: &OsStr) -> io::Result<()>;
}
