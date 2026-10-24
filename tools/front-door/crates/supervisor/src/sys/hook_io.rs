//! Bounded hook stdin without a detached reader or changes to inherited flags.
use std::{io, time::Instant};

/// Poll and consume at most cap+1 bytes. Hook entry points call this before
/// starting other stdin readers. A single monotonic deadline covers every chunk.
pub fn read_stdin(cap: usize, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "hook input timed out"))?;
        let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: fd is one live writable pollfd, borrowed only for this call.
        let ready = unsafe { libc::poll(&mut fd, 1, millis) };
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
        // SAFETY: stdin is borrowed, chunk has count writable bytes, and this
        // entry point is the sole reader. poll reported readiness or EOF/error.
        let read = unsafe { libc::read(libc::STDIN_FILENO, chunk.as_mut_ptr().cast(), count) };
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
    // SAFETY: fcntl borrows the live stdout descriptor, with no pointer args.
    let flags = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the flags are from F_GETFL; this only enables nonblocking I/O.
    if unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = write_ready(bytes, deadline);
    // SAFETY: restore exactly the flags read from the same borrowed descriptor.
    let restored = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, flags) };
    if restored < 0 && result.is_ok() {
        return Err(io::Error::last_os_error());
    }
    result
}
fn write_ready(mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "hook output timed out"))?;
        let mut fd = libc::pollfd {
            fd: libc::STDOUT_FILENO,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: fd is one writable pollfd, borrowed only for this call.
        let ready = unsafe {
            libc::poll(
                &mut fd,
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
        // SAFETY: bytes is live readable storage; stdout is borrowed, not closed.
        let count = unsafe { libc::write(libc::STDOUT_FILENO, bytes.as_ptr().cast(), bytes.len()) };
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
