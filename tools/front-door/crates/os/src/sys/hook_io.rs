//! Bounded hook stdin without a detached reader or changes to inherited flags.
use std::{io, os::fd::RawFd, time::Instant};

/// Poll and consume at most cap+1 bytes. Hook entry points call this before
/// starting other stdin readers. A single monotonic deadline covers every chunk.
pub fn read_stdin(cap: usize, deadline: Instant) -> io::Result<Vec<u8>> {
    read_fd_with(libc::STDIN_FILENO, cap, || {
        deadline.checked_duration_since(Instant::now())
    })
}

fn read_fd_with(
    fd: RawFd,
    cap: usize,
    mut remaining: impl FnMut() -> Option<std::time::Duration>,
) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let remaining = remaining()
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "hook input timed out"))?;
        let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: fd is one live writable pollfd, borrowed only for this call.
        let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        let mut chunk = [0u8; 8192];
        let count = chunk
            .len()
            .min(cap.saturating_add(1).saturating_sub(bytes.len()));
        // SAFETY: fd is borrowed, chunk has count writable bytes, and the
        // caller owns the read boundary. poll reported readiness or EOF/error.
        let read = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), count) };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read as usize]);
        if bytes.len() > cap {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "hook input exceeds limit",
            ));
        }
    }
}

/// Small protocol responses fit in a pipe's atomic write bound. Poll before
/// each write and use nonblocking mode so a stopped host reader cannot hang a
/// denial. Restore the inherited flags before returning.
pub fn write_stdout(bytes: &[u8], deadline: Instant) -> io::Result<()> {
    write_fd_with(libc::STDOUT_FILENO, bytes, || {
        deadline.checked_duration_since(Instant::now())
    })
}

fn write_fd_with(
    fd: RawFd,
    bytes: &[u8],
    mut remaining: impl FnMut() -> Option<std::time::Duration>,
) -> io::Result<()> {
    // SAFETY: fcntl borrows the live descriptor, with no pointer args.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the flags are from F_GETFL; this only enables nonblocking I/O.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = write_ready(fd, bytes, &mut remaining);
    // SAFETY: restore exactly the flags read from the same borrowed descriptor.
    let restored = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    if restored < 0 && result.is_ok() {
        return Err(io::Error::last_os_error());
    }
    result
}
fn write_ready(
    fd: RawFd,
    mut bytes: &[u8],
    remaining: &mut impl FnMut() -> Option<std::time::Duration>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let remaining = remaining()
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "hook output timed out"))?;
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: fd is one writable pollfd, borrowed only for this call.
        let ready = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
            )
        };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        // SAFETY: bytes is live readable storage; fd is borrowed, not closed.
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "hook response unavailable",
            ));
        }
        bytes = &bytes[count as usize..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    #[test]
    fn hook_input_returns_complete_frames_and_refuses_oversize() -> io::Result<()> {
        let (reader, mut writer) = io::pipe()?;
        writer.write_all(b"frame")?;
        drop(writer);
        let frame = read_fd_with(reader.as_raw_fd(), 5, || {
            Some(std::time::Duration::from_secs(1))
        })?;
        assert_eq!(frame, b"frame");

        let (reader, mut writer) = io::pipe()?;
        writer.write_all(b"frame")?;
        drop(writer);
        let error = read_fd_with(reader.as_raw_fd(), 4, || {
            Some(std::time::Duration::from_secs(1))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        Ok(())
    }

    #[test]
    fn hook_input_and_output_report_deadline_expiry_as_timed_out() -> io::Result<()> {
        let (reader, _writer) = io::pipe()?;
        let error = read_fd_with(reader.as_raw_fd(), 8, || None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);

        let (_reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd();
        // SAFETY: F_GETFL observes the borrowed live pipe writer.
        let original = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(original >= 0);
        let error = write_fd_with(fd, b"response", || None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        // SAFETY: F_GETFL observes the same borrowed live pipe writer.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFL) }, original);
        Ok(())
    }

    #[test]
    fn hook_output_writes_the_complete_response() -> io::Result<()> {
        let (mut reader, writer) = io::pipe()?;
        write_fd_with(writer.as_raw_fd(), b"response\n", || {
            Some(std::time::Duration::from_secs(1))
        })?;
        drop(writer);
        let mut response = Vec::new();
        reader.read_to_end(&mut response)?;
        assert_eq!(response, b"response\n");
        Ok(())
    }
}
