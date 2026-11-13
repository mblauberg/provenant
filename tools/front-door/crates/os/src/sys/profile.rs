//! Final Seatbelt transport. Custody owners verify the source before preparing
//! this pipe; only its read end crosses exec. No profile bytes enter argv.
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::process::Child;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Implemented by the owner of the verified, immutable profile file. This keeps
/// the utility executor below the custody layer without bypassing verification.
pub trait ProfileSource: Send + Sync {
    fn prepare(&self, argv: &[OsString]) -> io::Result<ProfilePipe>;
}

#[derive(Debug, thiserror::Error)]
#[error("sandbox profile transport refused: {0}")]
struct ProfileTransportError(String);

pub fn failure(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(ProfileTransportError(error.to_string()))
}

pub fn is_transport_error(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<ProfileTransportError>())
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sandbox_check(
        pid: libc::pid_t,
        operation: *const libc::c_char,
        filter_type: libc::c_int,
        ...
    ) -> libc::c_int;
}

/// Whether the calling process is already running inside an operating-system sandbox (Seatbelt).
pub fn current_process_is_sandboxed() -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: null operation with filter type 0 checks whether the calling process is sandboxed.
        unsafe { sandbox_check(0, std::ptr::null(), 0) == 1 }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub struct ProfilePipe {
    reader: io::PipeReader,
    bytes: Arc<[u8]>,
    argv: Vec<OsString>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<io::Result<()>>>,
    // A failed writer must not expose EOF on a valid but incomplete policy.
    // Keep this CLOEXEC duplicate until success, or until the child is stopped.
    eof_guard: Option<io::PipeWriter>,
    deadline: Instant,
    #[cfg(test)]
    writer_fd: i32,
}

impl ProfilePipe {
    /// `bytes` must be the complete verified snapshot. The source owner binds
    /// the incoming argv before calling this; the returned argv names this pipe.
    pub fn new(bytes: Vec<u8>, argv: &[OsString]) -> io::Result<Self> {
        Self::with_timeout(bytes, argv, Duration::from_secs(10))
    }

    fn with_timeout(bytes: Vec<u8>, argv: &[OsString], timeout: Duration) -> io::Result<Self> {
        Self::with_writer(bytes, argv, timeout, |writer| writer)
    }

    fn with_writer<W: Write + Send + 'static>(
        bytes: Vec<u8>,
        argv: &[OsString],
        timeout: Duration,
        wrap: impl FnOnce(io::PipeWriter) -> W,
    ) -> io::Result<Self> {
        if argv.len() < 4
            || argv[0] != "/usr/bin/sandbox-exec"
            || argv[1] != "-f"
            || argv[3].is_empty()
            || argv[3].as_encoded_bytes().starts_with(b"-")
        {
            return Err(failure("invalid confined command"));
        }
        let (reader, writer) = io::pipe().map_err(failure)?;
        let eof_guard = Some(writer.try_clone().map_err(failure)?);
        // The retained reader allows a consumption check, but also prevents
        // EPIPE on an early child exit. Nonblocking writes and cancellation are
        // essential: neither a failed spawn nor a partial reader may strand us.
        super::raw_exec::nonblocking(&writer).map_err(failure)?;
        #[cfg(test)]
        let writer_fd = writer.as_raw_fd();
        let mut writer = wrap(writer);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancelled);
        let bytes: Arc<[u8]> = bytes.into();
        let input = Arc::clone(&bytes);
        let deadline = Instant::now() + timeout;
        let worker = std::thread::Builder::new()
            .name("sandbox-profile".into())
            .spawn(move || {
                let mut written = 0;
                while written < input.len() {
                    if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
                        return Err(failure("profile writer cancelled or timed out"));
                    }
                    match writer.write(&input[written..]) {
                        Ok(0) => return Err(failure("profile pipe write returned zero")),
                        Ok(count) => written += count,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::park_timeout(Duration::from_millis(2));
                        }
                        Err(error) => {
                            return Err(failure(format!("profile pipe write failed: {error}")));
                        }
                    }
                }
                // Closing the writer supplies EOF, including for an empty profile.
                Ok(())
            })
            .map_err(failure)?;
        let mut argv = argv.to_vec();
        argv[2] = format!("/dev/fd/{}", reader.as_raw_fd()).into();
        Ok(Self {
            reader,
            bytes,
            argv,
            cancelled,
            worker: Some(worker),
            eof_guard,
            deadline,
            #[cfg(test)]
            writer_fd,
        })
    }

    pub fn argv(&self) -> &[OsString] {
        &self.argv
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.reader.as_fd()
    }

    /// Bind exactly this reader, never a caller-selected path or descriptor.
    pub fn verify_command(&self, argv: &[OsString]) -> io::Result<()> {
        if argv != self.argv {
            return Err(failure("command does not consume the held profile pipe"));
        }
        Ok(())
    }

    pub fn finish(mut self, child: &mut Child) -> io::Result<()> {
        let result = self.complete(child);
        if result.is_err() {
            // Stop before Drop releases the EOF guard: a prefix can itself be
            // valid SBPL, but must never be applied as the complete profile.
            // Do not reap; the process owner retains its PID/PGID reservation.
            let _ = child.kill();
        }
        result
    }

    fn complete(&mut self, child: &Child) -> io::Result<()> {
        loop {
            if self
                .worker
                .as_ref()
                .is_some_and(|worker| worker.is_finished())
            {
                self.join()?;
            }
            let mut pending: libc::c_int = 0;
            // SAFETY: reader is live and pending is writable integer storage.
            // FIONREAD only observes the pipe buffer and retains no pointer.
            if unsafe { libc::ioctl(self.reader.as_raw_fd(), libc::FIONREAD, &mut pending) } < 0 {
                return Err(failure(io::Error::last_os_error()));
            }
            if self.worker.is_none() && pending == 0 {
                return Ok(());
            }
            if super::raw_exec::child_exited(child.id()).map_err(failure)? {
                // The writer may have finished concurrently with the previous
                // check. Join it and recheck consumption before refusing.
                self.cancelled.store(true, Ordering::Release);
                self.join()?;
                let mut remaining: libc::c_int = 0;
                // SAFETY: as above; the writer is now joined, so no bytes can
                // arrive between this check and closing the parent's reader.
                if unsafe { libc::ioctl(self.reader.as_raw_fd(), libc::FIONREAD, &mut remaining) }
                    < 0
                {
                    return Err(failure(io::Error::last_os_error()));
                }
                return if remaining == 0 {
                    Ok(())
                } else {
                    Err(failure("sandbox-exec did not consume the whole profile"))
                };
            }
            if Instant::now() >= self.deadline {
                return Err(failure(
                    "sandbox-exec did not consume the whole profile before timeout",
                ));
            }
            std::thread::park_timeout(Duration::from_millis(2));
        }
    }

    fn join(&mut self) -> io::Result<()> {
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| failure("profile writer panicked"))??;
            self.eof_guard.take();
        }
        Ok(())
    }
}

impl Drop for ProfilePipe {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn argv() -> Vec<OsString> {
        vec![
            "/usr/bin/sandbox-exec".into(),
            "-f".into(),
            "/dev/fd/999".into(),
            "/bin/cat".into(),
        ]
    }

    // Medium: real pipe and exec boundary; no Seatbelt entitlement needed.
    #[test]
    fn pipe_transfers_large_profiles_and_refuses_partial_consumption() -> io::Result<()> {
        for size in [64, 700_181] {
            let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let output = tempfile::tempfile()?;
            let pipe = ProfilePipe::new(bytes.clone(), &argv())?;
            let mut command = Command::new("/bin/cat");
            command.arg(&pipe.argv()[2]).stdout(output.try_clone()?);
            super::super::child::configure_command(&mut command, &[pipe.fd()], None, false)?;
            let mut child = command.spawn()?;
            pipe.finish(&mut child)?;
            assert!(child.wait()?.success());
            use std::os::unix::fs::FileExt;
            let mut applied = vec![0; size];
            output.read_exact_at(&mut applied, 0)?;
            assert_eq!(output.metadata()?.len(), size as u64);
            assert_eq!(applied, bytes);

            for prefix in [false, true] {
                let pipe = ProfilePipe::new(bytes.clone(), &argv())?;
                let mut command = Command::new(if prefix { "/bin/dd" } else { "/usr/bin/true" });
                if prefix {
                    let mut input = OsString::from("if=");
                    input.push(&pipe.argv()[2]);
                    command.args([input, "bs=1".into(), "count=1".into()]);
                }
                command.stdout(Stdio::null()).stderr(Stdio::null());
                super::super::child::configure_command(&mut command, &[pipe.fd()], None, false)?;
                let mut child = command.spawn()?;
                let error = pipe.finish(&mut child).unwrap_err();
                assert!(is_transport_error(&error), "{error}");
                // finish must never reap: the custody owner retains the PID.
                assert!(super::super::raw_exec::child_exited(child.id())?);
                child.wait()?;
            }
        }
        Ok(())
    }

    // Medium: a reader kept open without reading and an exec failure must not
    // strand the writer, even when the snapshot exceeds the pipe's capacity.
    #[test]
    fn pipe_timeout_and_failed_spawn_join_the_writer() -> io::Result<()> {
        let pipe = ProfilePipe::with_timeout(
            vec![b';'; 1024 * 1024],
            &argv(),
            Duration::from_millis(100),
        )?;
        let mut command = Command::new("/bin/sleep");
        command.arg("5");
        super::super::child::configure_command(&mut command, &[pipe.fd()], None, false)?;
        let mut child = command.spawn()?;
        let error = pipe.finish(&mut child).unwrap_err();
        assert!(is_transport_error(&error), "{error}");
        child.kill()?;
        child.wait()?;

        let pipe = ProfilePipe::new(vec![b';'; 1024 * 1024], &argv())?;
        let mut command = Command::new("/nonexistent-profile-consumer");
        super::super::child::configure_command(&mut command, &[pipe.fd()], None, false)?;
        assert_eq!(command.spawn().unwrap_err().kind(), io::ErrorKind::NotFound);
        let input = Arc::downgrade(&pipe.bytes);
        let fd = pipe.fd().as_raw_fd();
        let started = Instant::now();
        drop(pipe);
        assert!(started.elapsed() < Duration::from_secs(2));
        // Drop joined the worker; no thread or descriptors escape the refusal.
        assert!(input.upgrade().is_none());
        // SAFETY: F_GETFD only queries this number. No owning wrapper is made
        // for the descriptor that Drop has already closed.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        Ok(())
    }

    // Medium: an actual pipe whose write syscall is faulted at the I/O edge.
    #[test]
    fn pipe_write_failure_never_applies_a_valid_prefix() -> io::Result<()> {
        struct FailedWriter {
            pipe: io::PipeWriter,
            wrote: bool,
        }
        impl Write for FailedWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.wrote {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                self.wrote = true;
                self.pipe
                    .write(&bytes[..b"(version 1)\n(allow default)\n".len()])
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut bytes = b"(version 1)\n(allow default)\n".to_vec();
        bytes.extend_from_slice(&vec![b';'; 700_181]);
        let pipe = ProfilePipe::with_writer(bytes, &argv(), Duration::from_secs(5), |pipe| {
            FailedWriter { pipe, wrote: false }
        })?;
        let input = Arc::downgrade(&pipe.bytes);
        // Observe the worker's failure before spawn: its prefix is available,
        // but EOF must remain withheld until the consumer has been stopped.
        let started = Instant::now();
        while !pipe
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::yield_now();
        }
        super::super::raw_exec::nonblocking(&pipe.reader)?;
        use std::io::Read;
        let mut reader = pipe.reader.try_clone()?;
        let mut prefix = vec![0; b"(version 1)\n(allow default)\n".len()];
        reader.read_exact(&mut prefix)?;
        assert_eq!(prefix, b"(version 1)\n(allow default)\n");
        assert_eq!(
            reader.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(reader);
        // SAFETY: this live pipe is privately owned by the test. F_SETFL
        // restores its original blocking mode without retaining a pointer.
        if unsafe { libc::fcntl(pipe.fd().as_raw_fd(), libc::F_SETFL, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let root = tempfile::tempdir()?;
        let marker = root.path().join("applied");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "while IFS= read -r line; do :; done < \"$1\"; printf applied > \"$2\"",
                "profile",
            ])
            .arg(&pipe.argv()[2])
            .arg(&marker)
            .stderr(Stdio::null());
        super::super::child::configure_command(&mut command, &[pipe.fd()], None, false)?;
        let mut child = command.spawn()?;
        let error = pipe.finish(&mut child).unwrap_err();
        assert!(!child.wait()?.success());
        assert!(!marker.exists());
        assert!(is_transport_error(&error));
        assert!(error.to_string().contains("profile pipe write failed"));
        assert!(input.upgrade().is_none());
        Ok(())
    }

    // Medium: re-exec a dedicated fixture before it opens any file handles.
    #[test]
    fn pipe_exec_inherits_only_the_reader() -> io::Result<()> {
        let source = tempfile::tempfile()?;
        let pipe = ProfilePipe::new(vec![b';'; 700_181], &argv())?;
        let mut command = Command::new(std::env::current_exe()?);
        let mut env: Vec<_> = std::env::vars_os().collect();
        env.push((
            "PROVENANT_PROFILE_PIPE_FIXTURE".into(),
            format!(
                "{} {} {} {}",
                pipe.fd().as_raw_fd(),
                pipe.writer_fd,
                pipe.eof_guard
                    .as_ref()
                    .ok_or_else(|| io::Error::other("missing EOF guard"))?
                    .as_raw_fd(),
                source.as_raw_fd()
            )
            .into(),
        ));
        command
            .args([
                "--exact",
                "sys::profile::tests::pipe_descriptor_fixture",
                "--nocapture",
            ])
            .stdout(Stdio::inherit());
        super::super::child::configure_command(&mut command, &[pipe.fd()], Some(&env), false)?;
        let mut child = command.spawn()?;
        pipe.finish(&mut child)?;
        assert!(child.wait()?.success());
        Ok(())
    }

    #[test]
    fn pipe_descriptor_fixture() -> io::Result<()> {
        let Ok(value) = std::env::var("PROVENANT_PROFILE_PIPE_FIXTURE") else {
            return Ok(());
        };
        let fds = value
            .split_whitespace()
            .map(str::parse::<i32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(io::Error::other)?;
        assert_eq!(fds.len(), 4);
        // SAFETY: F_GETFL only observes a descriptor number supplied by our
        // parent fixture, with no ownership transfer or retained pointer.
        let flags = unsafe { libc::fcntl(fds[0], libc::F_GETFL) };
        assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
        for &fd in &fds[1..] {
            // SAFETY: F_GETFD only observes a number; an absent fd is expected.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        }
        // SAFETY: this reader was explicitly inherited and remains live for
        // this borrow; duplicating it gives File its own owning description.
        let reader = unsafe { BorrowedFd::borrow_raw(fds[0]) }.try_clone_to_owned()?;
        let mut bytes = Vec::new();
        use std::io::Read;
        std::fs::File::from(reader).read_to_end(&mut bytes)?;
        assert_eq!(bytes, vec![b';'; 700_181]);
        Ok(())
    }
}
