//! One native event source multiplexes owner observation paths for a waiter.
use std::{
    io::{self, ErrorKind, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Inotify,
    Kqueue,
    Unsupported,
}
#[derive(Debug, Error)]
pub enum WatchError {
    #[error("filesystem event observation is unavailable on this host")]
    Unsupported,
    #[error("filesystem event observation failed: {0}")]
    Io(#[source] io::Error),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Changed,
    TimedOut,
    Woken,
}

/// A clonable cancellation signal that interrupts a blocking event wait.
#[derive(Clone)]
pub struct WakeHandle(Arc<UnixStream>);
impl WakeHandle {
    pub fn wake(&self) -> io::Result<()> {
        let mut stream = self.0.as_ref();
        match stream.write_all(&[1]) {
            Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(()),
            result => result,
        }
    }
}

/// Watch owner observation files or logs; notifications only prompt a state reread.
pub struct Watcher {
    native: crate::sys::changes::Watcher,
    wake: WakeHandle,
}
impl Watcher {
    /// Register any number of observation paths on one host event source. Parent
    /// directories are watched too, so creation or replacement rearms a path.
    pub fn subscribe<P: AsRef<Path>>(paths: &[P]) -> Result<Self, WatchError> {
        let (reader, writer) = UnixStream::pair().map_err(WatchError::Io)?;
        reader.set_nonblocking(true).map_err(WatchError::Io)?;
        writer.set_nonblocking(true).map_err(WatchError::Io)?;
        let native = crate::sys::changes::Watcher::subscribe(paths, reader).map_err(|error| {
            if error.kind() == io::ErrorKind::Unsupported {
                WatchError::Unsupported
            } else {
                WatchError::Io(error)
            }
        })?;
        Ok(Self {
            native,
            wake: WakeHandle(Arc::new(writer)),
        })
    }
    pub fn wake_handle(&self) -> WakeHandle {
        self.wake.clone()
    }
    pub fn wait(&mut self, timeout: Duration) -> Result<WaitOutcome, WatchError> {
        self.native.wait(timeout).map_err(WatchError::Io)
    }
    pub const fn backend(&self) -> Backend {
        crate::sys::changes::backend()
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn one_backend_observes_log_creation_and_later_append() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("first.events.jsonl");
        let second = directory.path().join("second.events.jsonl");
        std::fs::write(&first, b"")?;
        let mut watcher = Watcher::subscribe(&[first, second.clone()])
            .map_err(|error| io::Error::other(error.to_string()))?;
        std::fs::write(&second, b"created\n")?;
        assert_eq!(watcher.wait(Duration::from_secs(2))?, WaitOutcome::Changed);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&second)?
            .write_all(b"appended\n")?;
        assert_eq!(watcher.wait(Duration::from_secs(2))?, WaitOutcome::Changed);
        Ok(())
    }

    #[test]
    fn cancellation_wakes_the_native_wait() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let log = directory.path().join("graph.events.jsonl");
        std::fs::write(&log, b"")?;
        let mut watcher =
            Watcher::subscribe(&[log]).map_err(|error| io::Error::other(error.to_string()))?;
        let wake = watcher.wake_handle();
        let result = std::thread::scope(|scope| {
            let waiting = scope.spawn(move || watcher.wait(Duration::from_secs(60)));
            wake.wake()?;
            waiting
                .join()
                .map_err(|_| io::Error::other("event waiter panicked"))?
        })?;
        assert_eq!(result?, WaitOutcome::Woken);
        Ok(())
    }
}
