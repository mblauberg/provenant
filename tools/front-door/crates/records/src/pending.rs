use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

// port: belongs in provenant-core
// Path.resolve(strict=False): follow existing links, retaining missing components.
pub(crate) fn resolve(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut parts = components(&absolute);
    let mut resolved = PathBuf::new();
    let mut links = 0;
    while let Some(part) = parts.pop_front() {
        if part == "/" {
            resolved.push(Path::new("/"));
        } else if part == ".." {
            resolved.pop();
        } else if part != "." {
            resolved.push(&part);
            match std::fs::read_link(&resolved) {
                Ok(target) => {
                    links += 1;
                    if links > 40 {
                        return Err(io::Error::other("too many symbolic links"));
                    }
                    resolved.pop();
                    let mut target_parts = components(&target);
                    target_parts.append(&mut parts);
                    parts = target_parts;
                }
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                    // InvalidInput also reports a regular file on Unix. Reject NUL paths.
                    if part.as_encoded_bytes().contains(&0) {
                        return Err(error);
                    }
                }
                Err(_) => {}
            }
        }
    }
    Ok(resolved)
}

// port: belongs in provenant-core
fn components(path: &Path) -> VecDeque<OsString> {
    path.components()
        .map(|part| match part {
            Component::RootDir => OsString::from("/"),
            Component::CurDir => OsString::from("."),
            Component::ParentDir => OsString::from(".."),
            Component::Normal(name) => name.to_owned(),
            Component::Prefix(prefix) => prefix.as_os_str().to_owned(),
        })
        .collect()
}

// port: belongs in provenant-supervisor
/// Metadata captured without following a link. `allocated` is st_blocks * 512.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntryInfo {
    pub device: u64,
    pub inode: u64,
    pub links: u64,
    pub allocated: u64,
    pub directory: bool,
    pub symlink: bool,
}

// port: belongs in provenant-supervisor
/// Descriptor-relative operations. Handles own their descriptor and close on drop.
/// Implementations must not follow links in open_child; verified opens must also
/// compare before/after device and inode and require the anchor device.
pub(crate) trait StorageFs {
    type Dir;
    type Entries: Iterator<Item = io::Result<OsString>>;
    fn open_anchor(&self, path: &Path) -> io::Result<Self::Dir>;
    fn info(&self, dir: &Self::Dir) -> io::Result<EntryInfo>;
    fn stat(&self, parent: &Self::Dir, name: &std::ffi::OsStr) -> io::Result<EntryInfo>;
    fn open_child(&self, parent: &Self::Dir, name: &std::ffi::OsStr) -> io::Result<Self::Dir>;
    fn entries(&self, dir: &Self::Dir) -> io::Result<Self::Entries>;
    fn make_writable(&self, dir: &Self::Dir) -> io::Result<()>;
    fn unlink(&self, parent: &Self::Dir, name: &std::ffi::OsStr) -> io::Result<()>;
    fn rmdir(&self, parent: &Self::Dir, name: &std::ffi::OsStr) -> io::Result<()>;
}
