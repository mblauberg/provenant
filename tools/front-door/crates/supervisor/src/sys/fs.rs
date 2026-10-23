//! Filesystem FFI. All descriptors stay behind private File/Directory handles.
use crate::fs::{FileKind, Metadata};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

fn string(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}
fn status(result: i32) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn open_at(directory: i32, path: &OsStr, flags: i32, mode: u32) -> io::Result<File> {
    let path = string(path)?;
    // SAFETY: path is live and NUL-terminated; directory is a borrowed File's
    // descriptor or AT_FDCWD, valid through the call. openat retains no pointers
    // and atomically sets CLOEXEC. Each successful fd has one owner below;
    // concurrent calls are independent, failures are read from thread-local errno.
    let fd = unsafe {
        libc::openat(
            directory,
            path.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful openat transferred exactly one fresh fd to us.
    // No other owner exists; File closes it once on Drop.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(crate) fn open_anchor(path: &Path) -> io::Result<File> {
    open_at(
        libc::AT_FDCWD,
        path.as_os_str(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    )
}
pub(crate) fn open_directory(dir: &File, name: &OsStr, follow: bool) -> io::Result<File> {
    open_at(
        dir.as_raw_fd(),
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | if follow { 0 } else { libc::O_NOFOLLOW },
        0,
    )
}
pub(crate) fn open_read(dir: &File, name: &OsStr) -> io::Result<File> {
    open_at(
        dir.as_raw_fd(),
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )
}
pub(crate) fn open_source(path: &Path) -> io::Result<File> {
    open_at(
        libc::AT_FDCWD,
        path.as_os_str(),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )
}
pub(crate) fn create_new(dir: &File, name: &OsStr) -> io::Result<File> {
    open_at(
        dir.as_raw_fd(),
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o600,
    )
}
pub(crate) fn metadata_at(dir: &File, name: &OsStr, follow: bool) -> io::Result<Metadata> {
    let name = string(name)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: dir is borrowed and live, name is NUL-terminated, and stat has
    // the required size/alignment. fstatat initialises it only on success;
    // no ownership or pointers escape. Concurrent queries are independent.
    status(unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW },
        )
    })?;
    // SAFETY: successful fstatat above initialised every field of stat.
    let stat = unsafe { stat.assume_init() };
    #[cfg(target_os = "macos")]
    let ns = stat.st_mtime_nsec;
    #[cfg(target_os = "linux")]
    let ns = stat.st_mtime_nsec;
    #[allow(clippy::unnecessary_cast)] // libc field widths differ across OSes.
    Ok(Metadata {
        kind: match stat.st_mode & libc::S_IFMT {
            libc::S_IFREG => FileKind::Regular,
            libc::S_IFDIR => FileKind::Directory,
            libc::S_IFLNK => FileKind::Symlink,
            _ => FileKind::Other,
        },
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
        links: stat.st_nlink as u64,
        size: stat.st_size.max(0) as u64,
        allocated: (stat.st_blocks.max(0) as u64).saturating_mul(512),
        modified_ns: i128::from(stat.st_mtime) * 1_000_000_000 + i128::from(ns),
        uid: stat.st_uid,
        mode: stat.st_mode as u32,
    })
}
pub(crate) fn mkdir(dir: &File, name: &OsStr, mode: u32) -> io::Result<()> {
    let name = string(name)?;
    // SAFETY: borrowed fd and NUL-terminated name stay live; mkdirat retains
    // neither, transfers no ownership, and reports thread-local errno.
    status(unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) })
}
pub(crate) fn unlink(dir: &File, name: &OsStr, directory: bool) -> io::Result<()> {
    let name = string(name)?;
    // SAFETY: live borrowed fd/name; unlinkat acts on the directory entry, never
    // follows a final symlink, retains no memory and reports errno on failure.
    status(unsafe {
        libc::unlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    })
}
pub(crate) enum Rename {
    Replace,
    NoReplace,
    Exchange,
}
pub(crate) fn rename(
    from: &File,
    source: &OsStr,
    to: &File,
    destination: &OsStr,
    mode: Rename,
) -> io::Result<()> {
    let source = string(source)?;
    let destination = string(destination)?;
    // SAFETY: both fds are borrowed from live owners; both paths are live
    // NUL-terminated single components. renameat* retains no pointers, follows
    // no final symlinks, and atomically changes directory entries. Concurrent
    // contenders are decided by the kernel; errors leave ownership unchanged.
    let result = unsafe {
        match mode {
            Rename::Replace => libc::renameat(
                from.as_raw_fd(),
                source.as_ptr(),
                to.as_raw_fd(),
                destination.as_ptr(),
            ),
            #[cfg(target_os = "linux")]
            Rename::NoReplace | Rename::Exchange => libc::renameat2(
                from.as_raw_fd(),
                source.as_ptr(),
                to.as_raw_fd(),
                destination.as_ptr(),
                if matches!(mode, Rename::NoReplace) {
                    libc::RENAME_NOREPLACE
                } else {
                    libc::RENAME_EXCHANGE
                },
            ),
            #[cfg(target_os = "macos")]
            Rename::NoReplace | Rename::Exchange => libc::renameatx_np(
                from.as_raw_fd(),
                source.as_ptr(),
                to.as_raw_fd(),
                destination.as_ptr(),
                if matches!(mode, Rename::NoReplace) {
                    libc::RENAME_EXCL
                } else {
                    libc::RENAME_SWAP
                },
            ),
        }
    };
    status(result)
}
pub(crate) fn symlink(target: &Path, dir: &File, name: &OsStr) -> io::Result<()> {
    let target = string(target.as_os_str())?;
    let name = string(name)?;
    // SAFETY: strings and borrowed descriptor remain live; symlinkat copies
    // target bytes, retains no pointers and reports thread-local errno.
    status(unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) })
}
pub(crate) fn entropy() -> io::Result<[u8; 12]> {
    let mut bytes = [0; 12];
    // SAFETY: exclusive writable buffer is live and below getentropy's 256-byte
    // bound. It retains no pointer; concurrent calls share no Rust state.
    status(unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) })?;
    Ok(bytes)
}
struct Stream(*mut libc::DIR);
impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: fdopendir transferred unique ownership to this stream. All
        // readdir borrows ended; closedir frees it and closes its fd once.
        unsafe { libc::closedir(self.0) };
    }
}
pub(crate) fn entries(dir: &File) -> io::Result<Vec<OsString>> {
    // openat(.) gives an independent offset; dup would share enumeration state.
    let fresh = open_at(
        dir.as_raw_fd(),
        OsStr::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    )?;
    // SAFETY: fresh owns a live directory fd; fdopendir transfers ownership only
    // on success. No pointer is shared with another thread.
    let stream = unsafe { libc::fdopendir(fresh.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error());
    }
    std::mem::forget(fresh); // closedir owns its descriptor now.
    let stream = Stream(stream);
    let mut names = Vec::new();
    loop {
        // SAFETY: the stream is exclusively owned here. readdir returns null
        // at EOF or error, distinguished by clearing this thread's errno first.
        // A returned d_name is NUL-terminated and copied before the next call.
        unsafe {
            #[cfg(target_os = "macos")]
            let errno = libc::__error();
            #[cfg(target_os = "linux")]
            let errno = libc::__errno_location();
            *errno = 0;
            let entry = libc::readdir(stream.0);
            if entry.is_null() {
                return if *errno == 0 {
                    Ok(names)
                } else {
                    Err(io::Error::last_os_error())
                };
            }
            let name = CStr::from_ptr((*entry).d_name.as_ptr()).to_bytes();
            if name != b"." && name != b".." {
                names.push(OsString::from_vec(name.to_vec()));
            }
        }
    }
}
