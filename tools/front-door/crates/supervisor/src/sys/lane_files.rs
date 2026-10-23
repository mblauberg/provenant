//! Legacy lane descriptor operations, relocated without changing publication policy.
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

pub fn timestamp_epoch(sec: i64, millis: u32) -> String {
    // SAFETY: reentrant libc date conversion borrows live time/tm allocations;
    // no pointers escape, and the result is formatted before buffers drop.
    unsafe {
        let t = sec as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::gmtime_r(&t, &mut tm);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            i64::from(tm.tm_year) + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            millis
        )
    }
}
pub fn canonical_start(epoch: u64) -> String {
    // Match process_info._lstart and run-registry's C-locale ps identity.
    // SAFETY: reentrant libc date conversion borrows live time/tm allocations;
    // no pointers escape, and the result is formatted before buffers drop.
    unsafe {
        let t = epoch as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        let days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!(
            "{} {} {:2} {:02}:{:02}:{:02} {}",
            days.get(tm.tm_wday as usize).unwrap_or(&"?"),
            months.get(tm.tm_mon as usize).unwrap_or(&"?"),
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            i64::from(tm.tm_year) + 1900
        )
    }
}
fn cstr(s: &std::ffi::OsStr) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(s.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
}
fn cvt(fd: i32) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: successful openat yields one fresh owning fd, transferred exactly
        // once to File; failure is checked above, no concurrent owner exists.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

#[derive(Debug)]
pub struct LaneDirectory {
    dir: File,
    _lock: File,
}
impl LaneDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let name = cstr(std::ffi::OsStr::new("supervisor.lock"))?;
        // SAFETY: held live directory and NUL-terminated name are borrowed;
        // openat returns a fresh CLOEXEC owner or errno, never shared storage.
        let lock = cvt(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        })?;
        // SAFETY: lock is a private live file description; flock has no
        // pointers/ownership transfer and nonblocking acquisition returns errno.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { dir, _lock: lock })
    }
    fn parent(&self, path: &Path) -> io::Result<(File, CString)> {
        let mut parts = path.components().peekable();
        let mut dir = self.dir.try_clone()?;
        while let Some(part) = parts.next() {
            let Component::Normal(name) = part else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "uncontained file",
                ));
            };
            let name = cstr(name)?;
            if parts.peek().is_none() {
                return Ok((dir, name));
            }
            // SAFETY: live held parent and owned component remain valid;
            // no-follow directory open yields a fresh fd, owned once by File.
            dir = cvt(unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            })?;
        }
        Err(io::Error::new(io::ErrorKind::InvalidInput, "empty file"))
    }
    pub fn create(&self, path: &Path) -> io::Result<File> {
        let (dir, name) = self.parent(path)?;
        // SAFETY: validated parent/name are live, openat yields one new fd;
        // exclusive no-follow creation transfers no existing ownership.
        cvt(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        })
    }
    pub fn remove(&self, path: &str) {
        if let Ok((dir, name)) = self.parent(Path::new(path)) {
            // SAFETY: parent validates relative components and owns the held
            // directory; unlinkat borrows it and the live name, retaining none.
            unsafe {
                libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0);
            }
        }
    }
    pub fn exists(&self, path: &str) -> bool {
        let Ok((dir, name)) = self.parent(Path::new(path)) else {
            return false;
        };
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: validated parent/name and writable stat storage are live;
        // no pointer or descriptor ownership escapes, errno is thread-local.
        unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            ) == 0
        }
    }
    /// Publish a completed lane staging file. Durability policy stays in lane.
    pub fn publish_file(&self, from: &str, to: &str, no_replace: bool) -> io::Result<()> {
        let (source_dir, from) = self.parent(Path::new(from))?;
        let (destination_dir, to) = self.parent(Path::new(to))?;
        // SAFETY: both names and held directory descriptors stay live. linkat
        // preserves no-replace terminal publication; renameat replaces progress.
        // No pointer or ownership transfer occurs; errno is thread-local.
        let result = unsafe {
            if no_replace {
                libc::linkat(
                    source_dir.as_raw_fd(),
                    from.as_ptr(),
                    destination_dir.as_raw_fd(),
                    to.as_ptr(),
                    0,
                )
            } else {
                libc::renameat(
                    source_dir.as_raw_fd(),
                    from.as_ptr(),
                    destination_dir.as_raw_fd(),
                    to.as_ptr(),
                )
            }
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    pub fn sync(&self) -> io::Result<()> {
        self.dir.sync_all()
    }
}
/// Refresh a capped capture file's timestamps without writing discarded bytes.
pub fn touch(file: &File) -> io::Result<()> {
    // SAFETY: file owns a live borrowed fd; null times asks for current time.
    // futimens retains no pointers or descriptors and returns errno on failure.
    if unsafe { libc::futimens(file.as_raw_fd(), std::ptr::null()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_lane_paths_cannot_escape_the_held_directory() {
        let root = tempfile::tempdir().unwrap();
        let held = root.path().join("held");
        std::fs::create_dir(&held).unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"outside").unwrap();
        let dir = LaneDirectory::open(&held).unwrap();
        dir.remove("../outside");
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
        assert!(!dir.exists("../outside"));
        assert!(!dir.exists(outside.to_str().unwrap()));
        dir.create(Path::new("staging"))
            .unwrap()
            .write_all(b"staged")
            .unwrap();
        assert!(dir.publish_file("staging", "../outside", false).is_err());
        assert!(
            dir.publish_file("staging", outside.to_str().unwrap(), false)
                .is_err()
        );
        assert!(dir.publish_file("../outside", "inside", false).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    }
    use std::io::Write;
}
