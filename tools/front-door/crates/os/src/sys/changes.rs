//! Native filesystem-event backends. Unsafe calls stay behind this narrow port.
use std::os::unix::net::UnixStream;
use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };
    pub struct Watcher {
        event: std::fs::File,
        paths: Vec<PathBuf>,
        wake: UnixStream,
    }
    impl Watcher {
        pub fn subscribe<P: AsRef<Path>>(paths: &[P], wake: UnixStream) -> io::Result<Self> {
            if paths.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "no event logs"));
            }
            // SAFETY: successful inotify_init1 returns a fresh owned descriptor.
            let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is fresh and becomes solely owned here.
            let event = unsafe { std::fs::File::from_raw_fd(fd) };
            let paths = paths
                .iter()
                .map(|path| path.as_ref().to_owned())
                .collect::<Vec<_>>();
            let mut watcher = Self { event, paths, wake };
            watcher.rearm()?;
            Ok(watcher)
        }
        fn add(&self, path: &Path, mask: u32, missing_ok: bool) -> io::Result<()> {
            let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "event path contains NUL")
            })?;
            // SAFETY: descriptor is live; path is a NUL-terminated string borrowed for this call.
            let result =
                unsafe { libc::inotify_add_watch(self.event.as_raw_fd(), path.as_ptr(), mask) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if missing_ok && error.kind() == io::ErrorKind::NotFound {
                    return Ok(());
                }
                return Err(error);
            }
            Ok(())
        }
        fn rearm(&mut self) -> io::Result<()> {
            let directory_mask = libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF;
            let file_mask = libc::IN_MODIFY
                | libc::IN_ATTRIB
                | libc::IN_CLOSE_WRITE
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF
                | libc::IN_DONT_FOLLOW;
            for path in &self.paths {
                let parent = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                self.add(parent, directory_mask, false)?;
                self.add(path, file_mask, true)?;
            }
            Ok(())
        }
        pub fn wait(
            &mut self,
            timeout: Duration,
        ) -> io::Result<crate::changes::watch::WaitOutcome> {
            let end = Instant::now().checked_add(timeout);
            loop {
                let left = end.map_or(timeout, |d| d.saturating_duration_since(Instant::now()));
                if left.is_zero() {
                    return Ok(crate::changes::watch::WaitOutcome::TimedOut);
                }
                let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
                let mut fds = [
                    libc::pollfd {
                        fd: self.event.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: self.wake.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // SAFETY: fds is a live two-element writable array over owned descriptors.
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if ready == 0 {
                    return Ok(crate::changes::watch::WaitOutcome::TimedOut);
                }
                if fds[1].revents != 0 {
                    drain_wake(&mut self.wake)?;
                    return Ok(crate::changes::watch::WaitOutcome::Woken);
                }
                if fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    return Err(io::Error::other("inotify source closed"));
                }
                let mut bytes = [0u8; 8192];
                // SAFETY: bytes is writable and poll reported the event descriptor readable.
                let count = unsafe {
                    libc::read(
                        self.event.as_raw_fd(),
                        bytes.as_mut_ptr().cast(),
                        bytes.len(),
                    )
                };
                if count < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted
                        || error.kind() == io::ErrorKind::WouldBlock
                    {
                        continue;
                    }
                    return Err(error);
                }
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "inotify source closed",
                    ));
                }
                self.rearm()?;
                return Ok(crate::changes::watch::WaitOutcome::Changed);
            }
        }
    }
    fn drain_wake(wake: &mut UnixStream) -> io::Result<()> {
        use std::io::Read;
        let mut bytes = [0u8; 64];
        loop {
            match wake.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "event wake closed",
                    ));
                }
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
    pub const fn backend() -> crate::changes::watch::Backend {
        crate::changes::watch::Backend::Inotify
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::{
        collections::BTreeSet,
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };
    pub struct Watcher {
        queue: std::fs::File,
        paths: Vec<PathBuf>,
        directories: Vec<std::fs::File>,
        files: Vec<std::fs::File>,
        wake: UnixStream,
    }
    impl Watcher {
        pub fn subscribe<P: AsRef<Path>>(paths: &[P], wake: UnixStream) -> io::Result<Self> {
            if paths.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "no event logs"));
            }
            // SAFETY: kqueue has no pointer arguments and returns a fresh descriptor.
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful kqueue returns a fresh descriptor now owned here.
            let queue = unsafe { std::fs::File::from_raw_fd(fd) };
            // SAFETY: fcntl only updates descriptor flags on this owned queue.
            if unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let paths = paths
                .iter()
                .map(|path| path.as_ref().to_owned())
                .collect::<Vec<_>>();
            let mut value = Self {
                queue,
                paths,
                directories: Vec::new(),
                files: Vec::new(),
                wake,
            };
            value.rearm()?;
            value.watch_read_fd(value.wake.as_raw_fd())?;
            Ok(value)
        }
        fn open_watch(path: &Path) -> io::Result<std::fs::File> {
            let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "event path contains NUL")
            })?;
            // SAFETY: path is a valid C string; open returns a fresh owned descriptor.
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful open returns the sole owned descriptor.
            Ok(unsafe { std::fs::File::from_raw_fd(fd) })
        }
        fn watch_read_fd(&self, fd: i32) -> io::Result<()> {
            // SAFETY: zero is a valid empty kevent record, initialised below.
            let mut change: libc::kevent = unsafe { std::mem::zeroed() };
            change.ident = fd as usize;
            change.filter = libc::EVFILT_READ;
            change.flags = libc::EV_ADD | libc::EV_CLEAR;
            // SAFETY: queue/read fd are live and change is borrowed for this syscall only.
            if unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        fn watch_vnode(&self, file: &std::fs::File) -> io::Result<()> {
            // SAFETY: zero is a valid empty kevent record, initialised below.
            let mut change: libc::kevent = unsafe { std::mem::zeroed() };
            change.ident = file.as_raw_fd() as usize;
            change.filter = libc::EVFILT_VNODE;
            change.flags = libc::EV_ADD | libc::EV_CLEAR;
            change.fflags =
                libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE;
            // SAFETY: queue and file are live; change is borrowed for this call only.
            if unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        fn rearm(&mut self) -> io::Result<()> {
            let dirs = self
                .paths
                .iter()
                .map(|path| {
                    path.parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or(Path::new("."))
                        .to_owned()
                })
                .collect::<BTreeSet<_>>();
            let mut new_dirs = Vec::with_capacity(dirs.len());
            for path in dirs {
                let file = Self::open_watch(&path)?;
                self.watch_vnode(&file)?;
                new_dirs.push(file)
            }
            let mut new_files = Vec::with_capacity(self.paths.len());
            for path in &self.paths {
                match Self::open_watch(path) {
                    Ok(file) => {
                        self.watch_vnode(&file)?;
                        new_files.push(file)
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            self.directories = new_dirs;
            self.files = new_files;
            Ok(())
        }
        pub fn wait(
            &mut self,
            timeout: Duration,
        ) -> io::Result<crate::changes::watch::WaitOutcome> {
            let end = Instant::now().checked_add(timeout);
            loop {
                let left = end.map_or(timeout, |d| d.saturating_duration_since(Instant::now()));
                if left.is_zero() {
                    return Ok(crate::changes::watch::WaitOutcome::TimedOut);
                }
                let pause = libc::timespec {
                    tv_sec: libc::time_t::try_from(left.as_secs()).unwrap_or(libc::time_t::MAX),
                    tv_nsec: left.subsec_nanos() as libc::c_long,
                };
                // SAFETY: zeroed records are writable output slots; queue is live and timeout finite.
                let mut events: [libc::kevent; 16] = unsafe { std::mem::zeroed() };
                // SAFETY: events is an output array; no changes are submitted.
                let count = unsafe {
                    libc::kevent(
                        self.queue.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        events.as_mut_ptr(),
                        events.len() as libc::c_int,
                        &pause,
                    )
                };
                if count < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if count == 0 {
                    return Ok(crate::changes::watch::WaitOutcome::TimedOut);
                }
                for event in events.iter().take(count as usize) {
                    if event.flags & libc::EV_ERROR != 0 {
                        return Err(io::Error::from_raw_os_error(
                            i32::try_from(event.data).unwrap_or(libc::EIO),
                        ));
                    }
                    if event.filter == libc::EVFILT_READ
                        && event.ident == self.wake.as_raw_fd() as usize
                    {
                        drain_wake(&mut self.wake)?;
                        return Ok(crate::changes::watch::WaitOutcome::Woken);
                    }
                }
                self.rearm()?;
                return Ok(crate::changes::watch::WaitOutcome::Changed);
            }
        }
    }
    fn drain_wake(wake: &mut UnixStream) -> io::Result<()> {
        use std::io::Read;
        let mut bytes = [0u8; 64];
        loop {
            match wake.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "event wake closed",
                    ));
                }
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
    pub const fn backend() -> crate::changes::watch::Backend {
        crate::changes::watch::Backend::Kqueue
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;
    pub struct Watcher;
    impl Watcher {
        pub fn subscribe<P: AsRef<Path>>(_: &[P], _: UnixStream) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no filesystem event backend for host",
            ))
        }
        pub fn wait(&mut self, _: Duration) -> io::Result<crate::changes::watch::WaitOutcome> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no filesystem event backend for host",
            ))
        }
    }
    pub const fn backend() -> crate::changes::watch::Backend {
        crate::changes::watch::Backend::Unsupported
    }
}
pub use imp::Watcher;
pub const fn backend() -> crate::changes::watch::Backend {
    imp::backend()
}
