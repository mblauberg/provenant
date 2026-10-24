//! Filesystem FFI. All descriptors stay behind private File/Directory handles.
use crate::fs::{FileKind, Metadata};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

pub(crate) fn canonicalize(path: &Path) -> io::Result<std::path::PathBuf> {
    std::fs::canonicalize(path)
}

pub(crate) fn resolve_existing_ancestor(path: &Path) -> io::Result<std::path::PathBuf> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match canonicalize(existing) {
            Ok(mut bound) => {
                for name in missing.iter().rev() {
                    bound.push(name);
                }
                return Ok(bound);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or(error)?;
                if !matches!(
                    existing.components().next_back(),
                    Some(std::path::Component::Normal(_))
                ) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid missing path component",
                    ));
                }
                missing.push(name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "missing path parent")
                })?;
            }
            Err(error) => return Err(error),
        }
    }
}

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
// Retry only calls whose failure has no completed result to consume.
fn retry(mut call: impl FnMut() -> i32) -> io::Result<i32> {
    loop {
        let result = call();
        if result >= 0 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}
fn open_at(directory: i32, path: &OsStr, flags: i32, mode: u32) -> io::Result<File> {
    let path = string(path)?;
    // SAFETY: path is live and NUL-terminated; directory is a borrowed File's
    // descriptor or AT_FDCWD, valid through the call. openat retains no pointers
    // and atomically sets CLOEXEC. Each successful fd has one owner below;
    // concurrent calls are independent, failures are read from thread-local errno.
    let fd = retry(|| unsafe {
        libc::openat(
            directory,
            path.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    })
    .map_err(|error| {
        // Linux reports ENOTDIR for O_DIRECTORY|O_NOFOLLOW on a link, Darwin
        // ELOOP. Inspect the final entry without following it and canonicalise
        // only a positively identified symlink. Classification grants no access
        // and the entry may still change after this observation.
        if flags & libc::O_NOFOLLOW != 0
            && matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP))
        {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: live directory/name and correctly aligned output. Read the
            // output only after success; no ownership or pointers escape.
            if unsafe {
                libc::fstatat(
                    directory,
                    path.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } == 0
            {
                // SAFETY: successful fstatat initialised stat.
                if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFLNK {
                    return io::Error::from_raw_os_error(libc::ELOOP);
                }
            }
        }
        error
    })?;
    // SAFETY: the successful openat transferred exactly one fresh fd to us.
    // No other owner exists; File closes it once on Drop.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(crate) fn open_anchor(path: &Path) -> io::Result<File> {
    // Trailing / asks the kernel to follow a symlink as a directory even with
    // O_NOFOLLOW. Remove separators first while preserving the root path.
    let mut bytes = path.as_os_str().as_bytes();
    while bytes.len() > 1 && bytes.last() == Some(&b'/') {
        bytes = &bytes[..bytes.len() - 1];
    }
    open_at(
        libc::AT_FDCWD,
        OsStr::from_bytes(bytes),
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
pub(crate) fn open_lock(dir: &File, name: &OsStr) -> io::Result<File> {
    open_at(
        dir.as_raw_fd(),
        name,
        libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0o600,
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
pub(crate) fn open_append_at(dir: &File, name: &OsStr) -> io::Result<File> {
    open_at(
        dir.as_raw_fd(),
        name,
        libc::O_RDWR | libc::O_CREAT | libc::O_APPEND | libc::O_NOFOLLOW | libc::O_NONBLOCK,
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
    retry(|| unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) })
        .map(|_| ())
}
pub(crate) fn unlink(dir: &File, name: &OsStr, directory: bool) -> io::Result<()> {
    let name = string(name)?;
    // SAFETY: live borrowed fd/name; unlinkat acts on the directory entry, never
    // follows a final symlink, retains no memory and reports errno on failure.
    retry(|| unsafe {
        libc::unlinkat(
            dir.as_raw_fd(),
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    })
    .map(|_| ())
}
pub(crate) enum Rename {
    Replace,
    NoReplace,
    // Kept crate-private until an authorised caller lands.
    #[allow(dead_code)]
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
    retry(|| match mode {
        Rename::Replace => {
            // SAFETY: both fds are borrowed from live owners and both paths are
            // live NUL-terminated components. renameat retains no pointers and
            // atomically changes the directory entry.
            unsafe {
                libc::renameat(
                    from.as_raw_fd(),
                    source.as_ptr(),
                    to.as_raw_fd(),
                    destination.as_ptr(),
                )
            }
        }
        #[cfg(target_os = "linux")]
        Rename::NoReplace | Rename::Exchange => {
            // SAFETY: both fds are borrowed from live owners and both paths are
            // live NUL-terminated components. renameat2 retains no pointers
            // and atomically applies the selected exchange mode.
            unsafe {
                libc::renameat2(
                    from.as_raw_fd(),
                    source.as_ptr(),
                    to.as_raw_fd(),
                    destination.as_ptr(),
                    if matches!(mode, Rename::NoReplace) {
                        libc::RENAME_NOREPLACE
                    } else {
                        libc::RENAME_EXCHANGE
                    },
                )
            }
        }
        #[cfg(target_os = "macos")]
        Rename::NoReplace | Rename::Exchange => {
            // SAFETY: both fds are borrowed from live owners and both paths are
            // live NUL-terminated components. renameatx_np retains no pointers
            // and atomically applies the selected exchange mode.
            unsafe {
                libc::renameatx_np(
                    from.as_raw_fd(),
                    source.as_ptr(),
                    to.as_raw_fd(),
                    destination.as_ptr(),
                    if matches!(mode, Rename::NoReplace) {
                        libc::RENAME_EXCL
                    } else {
                        libc::RENAME_SWAP
                    },
                )
            }
        }
    })
    .map(|_| ())
}
pub(crate) fn symlink(target: &Path, dir: &File, name: &OsStr) -> io::Result<()> {
    let target = string(target.as_os_str())?;
    let name = string(name)?;
    // SAFETY: strings and borrowed descriptor remain live; symlinkat copies
    // target bytes, retains no pointers and reports thread-local errno.
    status(unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) })
}
pub(crate) fn entropy<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    // SAFETY: exclusive writable buffer is live and below getentropy's 256-byte
    // bound. It retains no pointer; concurrent calls share no Rust state.
    status(unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) })?;
    Ok(bytes)
}
pub(crate) struct Entries {
    stream: *mut libc::DIR,
    finished: bool,
}
impl Drop for Entries {
    fn drop(&mut self) {
        // SAFETY: fdopendir transferred unique ownership to this iterator.
        // Each readdir borrow ends before next()/Drop; no stream is shared.
        // closedir frees the stream and closes its owned descriptor once.
        unsafe { libc::closedir(self.stream) };
    }
}
impl Iterator for Entries {
    type Item = io::Result<OsString>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            // SAFETY: returns this thread's live errno slot, which is read and
            // written only during this call to next().
            #[cfg(target_os = "macos")]
            let errno = unsafe { libc::__error() };
            #[cfg(target_os = "linux")]
            // SAFETY: returns this thread's live errno slot for this call.
            let errno = unsafe { libc::__errno_location() };
            // SAFETY: errno is the current thread's live errno slot.
            unsafe { *errno = 0 };
            // SAFETY: &mut self gives exclusive access to the owned stream;
            // readdir's returned entry is borrowed until the next call.
            let entry = unsafe { libc::readdir(self.stream) };
            if entry.is_null() {
                self.finished = true;
                // SAFETY: errno remains the current thread's live errno slot.
                return if unsafe { *errno } == 0 {
                    None
                } else {
                    Some(Err(io::Error::last_os_error()))
                };
            }
            // SAFETY: entry is non-null and valid until the next readdir call;
            // d_name is the directory record's NUL-terminated byte array.
            let name_ptr = unsafe { (*entry).d_name.as_ptr() };
            // SAFETY: name_ptr points to the NUL-terminated d_name field of
            // this live directory entry; bytes are copied before next().
            let name = unsafe { CStr::from_ptr(name_ptr) }.to_bytes();
            if name != b"." && name != b".." {
                return Some(Ok(OsString::from_vec(name.to_vec())));
            }
        }
    }
}
pub(crate) fn entries(dir: &File) -> io::Result<Entries> {
    // openat(.) gives an independent offset; dup would share enumeration state.
    let fresh = open_at(
        dir.as_raw_fd(),
        OsStr::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    )?;
    // SAFETY: fresh owns a live directory fd; fdopendir transfers ownership only
    // on success. No pointer is shared with another iterator or thread.
    let stream = unsafe { libc::fdopendir(fresh.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error());
    }
    let _ = fresh.into_raw_fd(); // closedir owns its descriptor now.
    Ok(Entries {
        stream,
        finished: false,
    })
}

/// Called only after fstat has verified a regular file; FIFOs are refused first.
pub(crate) fn clear_nonblocking(file: &File) -> io::Result<()> {
    // SAFETY: borrowed live fd; fcntl retains no pointers or ownership. The file
    // has not escaped yet, so flags cannot race another user of this description.
    let flags = retry(|| unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) })?;
    // SAFETY: same live fd, preserving all status flags other than O_NONBLOCK.
    retry(|| unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) })
        .map(|_| ())
}

pub(crate) fn open_append(path: &Path, mode: u32) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NONBLOCK prevents a FIFO open from waiting before regular-file refusal.
    std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(mode)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
}
pub(crate) fn open_following(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
}
pub(crate) fn try_flock(file: &File, mode: crate::fs::LockMode) -> io::Result<()> {
    let op = match mode {
        crate::fs::LockMode::Shared => libc::LOCK_SH,
        crate::fs::LockMode::Exclusive => libc::LOCK_EX,
    } | libc::LOCK_NB;
    // SAFETY: file owns a live descriptor throughout this single nonblocking
    // flock call. It changes only that open file description's advisory lock,
    // retains no pointers and transfers no ownership. EINTR is returned so the
    // safe owner can retry within its WaitBudget.
    status(unsafe { libc::flock(file.as_raw_fd(), op) })
}
pub(crate) fn effective_uid() -> u32 {
    // SAFETY: geteuid reads only the calling process's effective UID and has no
    // pointer, ownership, lifetime or concurrency requirements.
    unsafe { libc::geteuid() }
}

pub(crate) fn wait(duration: std::time::Duration) {
    std::thread::sleep(duration);
}

/// Read a link's native bytes without following the target. The explicit cap
/// refuses truncation, including an entry that grows during inspection.
pub(crate) fn read_link(directory: &File, name: &OsStr) -> io::Result<std::path::PathBuf> {
    let name = string(name)?;
    let mut bytes = vec![0u8; 65_537];
    // SAFETY: directory is borrowed and live, name is NUL-terminated and bytes
    // is writable for its full length. No pointers or ownership escape; the
    // syscall is independent of concurrent readers and errno is thread-local.
    let count = unsafe {
        libc::readlinkat(
            directory.as_raw_fd(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    let count =
        usize::try_from(count).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    if count >= bytes.len() {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    bytes.truncate(count);
    Ok(OsString::from_vec(bytes).into())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn mkfifo(path: &Path) -> io::Result<()> {
        let path = string(path.as_os_str())?;
        // SAFETY: owned NUL-terminated path lives through mkfifo, which retains
        // no pointer and creates no owning descriptor. Errors use thread errno.
        status(unsafe { libc::mkfifo(path.as_ptr(), 0o600) })
    }
    pub(crate) fn is_nonblocking(file: &File) -> bool {
        // SAFETY: read-only fcntl on a live borrowed fd, no ownership transfer.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        flags & libc::O_NONBLOCK != 0
    }
    #[test]
    fn retry_repeats_only_interrupted_calls() {
        let mut calls = 0;
        let result = retry(|| {
            calls += 1;
            if calls == 1 {
                // SAFETY: platform accessor returns this thread's live errno
                // slot; no pointer escapes or aliases another thread.
                #[cfg(target_os = "macos")]
                let slot = unsafe { libc::__error() };
                #[cfg(target_os = "linux")]
                // SAFETY: returns this thread's live errno slot for this call.
                let slot = unsafe { libc::__errno_location() };
                // SAFETY: slot is the live thread-local errno location above.
                unsafe { *slot = libc::EINTR };
                -1
            } else {
                7
            }
        })
        .unwrap();
        assert_eq!(result, 7);
        assert_eq!(calls, 2);
        let mut calls = 0;
        let error = retry(|| {
            calls += 1;
            // SAFETY: platform accessor returns this thread's live errno
            // slot; no pointer escapes or aliases another thread.
            #[cfg(target_os = "macos")]
            let slot = unsafe { libc::__error() };
            #[cfg(target_os = "linux")]
            // SAFETY: returns this thread's live errno slot for this call.
            let slot = unsafe { libc::__errno_location() };
            // SAFETY: slot is the live thread-local errno location above.
            unsafe { *slot = libc::EACCES };
            -1
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    }
    #[test]
    fn anchors_opened_directories_and_files_are_all_cloexec() {
        let temp = tempfile::tempdir().unwrap();
        let anchor = open_anchor(temp.path()).unwrap();
        mkdir(&anchor, OsStr::new("child"), 0o700).unwrap();
        let child = open_directory(&anchor, OsStr::new("child"), false).unwrap();
        let new = create_new(&child, OsStr::new("file")).unwrap();
        let read = open_read(&child, OsStr::new("file")).unwrap();
        let source = open_source(&temp.path().join("child/file")).unwrap();
        for file in [&anchor, &child, &new, &read, &source] {
            // SAFETY: file owns a live descriptor and remains borrowed through
            // this read-only fcntl; no ownership transfers or concurrent close.
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0);
            assert_ne!(flags & libc::FD_CLOEXEC, 0);
        }
    }
}
