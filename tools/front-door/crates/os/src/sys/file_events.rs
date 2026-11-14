//! Descriptor-owned filesystem events. No transition polling or background thread.
use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::{Duration, Instant},
};

pub struct FileEvents {
    events: OwnedFd,
    // Retain the watched inode even if its directory is renamed.
    _directory: File,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_between_registration_and_wait_is_retained() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let handle = File::open(directory.path())?;
        let mut events = FileEvents::new(&handle)?;
        std::fs::write(directory.path().join("settled"), b"terminal")?;
        assert!(events.wait(Duration::from_secs(1))?);
        // A separate registration has no pending event and honours its budget.
        let mut quiet = FileEvents::new(&handle)?;
        assert!(!quiet.wait(Duration::from_millis(1))?);
        Ok(())
    }
}

impl FileEvents {
    pub(crate) fn new(directory: &File) -> io::Result<Self> {
        let directory = directory.try_clone()?;
        #[cfg(target_os = "macos")]
        let events = {
            // SAFETY: kqueue has no pointer arguments and returns a fresh fd.
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is fresh and exclusively owned here.
            let events = unsafe { OwnedFd::from_raw_fd(fd) };
            // SAFETY: fcntl borrows our open descriptor; no pointers are used.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let change = libc::kevent {
                ident: directory.as_raw_fd() as libc::uintptr_t,
                filter: libc::EVFILT_VNODE,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: libc::NOTE_WRITE | libc::NOTE_RENAME | libc::NOTE_DELETE,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: change is live for one input element; no output requested.
            if unsafe { libc::kevent(fd, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) }
                < 0
            {
                return Err(io::Error::last_os_error());
            }
            events
        };
        #[cfg(target_os = "linux")]
        let events = {
            // SAFETY: inotify_init1 takes flags and returns a fresh descriptor.
            let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: fd is fresh and exclusively owned here.
            let events = unsafe { OwnedFd::from_raw_fd(fd) };
            let path = std::ffi::CString::new(format!("/proc/self/fd/{}", directory.as_raw_fd()))?;
            // SAFETY: path is NUL terminated; the kernel borrows it during the call.
            if unsafe {
                libc::inotify_add_watch(
                    fd,
                    path.as_ptr(),
                    libc::IN_CREATE
                        | libc::IN_MOVED_TO
                        | libc::IN_DELETE
                        | libc::IN_CLOSE_WRITE
                        | libc::IN_DELETE_SELF
                        | libc::IN_MOVE_SELF,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            events
        };
        Ok(Self {
            events,
            _directory: directory,
        })
    }

    /// False only when the budget expires. Arm, read, then wait to avoid a
    /// settlement between the read and registration being missed.
    #[allow(clippy::disallowed_methods)] // Native event transport owns its deadline.
    pub fn wait(&mut self, within: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + within;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            #[cfg(target_os = "macos")]
            let result = {
                let timeout = libc::timespec {
                    tv_sec: remaining.as_secs().try_into().unwrap_or(libc::time_t::MAX),
                    tv_nsec: remaining.subsec_nanos().into(),
                };
                let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
                // SAFETY: event has space for one output; timeout is borrowed.
                unsafe {
                    libc::kevent(
                        self.events.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        event.as_mut_ptr(),
                        1,
                        &timeout,
                    )
                }
            };
            #[cfg(target_os = "linux")]
            let result = {
                let mut fd = libc::pollfd {
                    fd: self.events.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let millis = remaining
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32;
                // SAFETY: fd is one valid, borrowed pollfd element.
                let result = unsafe { libc::poll(&mut fd, 1, millis) };
                if result > 0 {
                    let mut buffer = [0_u8; 8192];
                    // SAFETY: buffer is writable for its full length; the fd is owned.
                    let read = unsafe {
                        libc::read(
                            self.events.as_raw_fd(),
                            buffer.as_mut_ptr().cast(),
                            buffer.len(),
                        )
                    };
                    if read < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                result
            };
            if result >= 0 {
                return Ok(result > 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}
