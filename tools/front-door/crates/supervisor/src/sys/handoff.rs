//! The private same-binary guardian channel. Only this edge inherits leases.
use crate::custody::{CustodyError, Result};
use crate::fs::{Directory, LockGuard};
use provenant_core::CancelToken;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// A 1 MiB stdin expands to 4 MiB as a JSON byte array; leave room for argv/env.
pub(crate) const MESSAGE_LIMIT: usize = 8 * 1024 * 1024;
pub(crate) struct Inherited {
    pub channel: UnixStream,
    pub writer_directory: Directory,
    pub attempt_directory: Directory,
    pub writer: LockGuard,
    pub custody: LockGuard,
}
fn deadline(timeout: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::ErrorKind::InvalidInput.into())
}
fn transfer(
    length: usize,
    mut operation: impl FnMut(usize) -> io::Result<usize>,
    deadline: Instant,
    cancel: &CancelToken,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < length {
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let result = operation(offset);
        match result {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => offset += count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
pub(crate) fn send(
    stream: &mut UnixStream,
    bytes: &[u8],
    timeout: Duration,
    cancel: &CancelToken,
) -> io::Result<()> {
    if bytes.len() > MESSAGE_LIMIT {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    stream.set_nonblocking(true)?;
    let deadline = deadline(timeout)?;
    let length = (bytes.len() as u64).to_be_bytes();
    transfer(
        length.len(),
        |offset| stream.write(&length[offset..]),
        deadline,
        cancel,
    )?;
    transfer(
        bytes.len(),
        |offset| stream.write(&bytes[offset..]),
        deadline,
        cancel,
    )
}
pub(crate) fn receive(stream: &mut UnixStream, timeout: Duration) -> io::Result<Vec<u8>> {
    let cancel = CancelToken::new();
    receive_cancellable(stream, timeout, &cancel)
}
pub(crate) fn receive_cancellable(
    stream: &mut UnixStream,
    timeout: Duration,
    cancel: &CancelToken,
) -> io::Result<Vec<u8>> {
    stream.set_nonblocking(true)?;
    let deadline = deadline(timeout)?;
    let mut length = [0_u8; 8];
    transfer(
        length.len(),
        |offset| stream.read(&mut length[offset..]),
        deadline,
        cancel,
    )?;
    let length =
        usize::try_from(u64::from_be_bytes(length)).map_err(|_| io::ErrorKind::InvalidData)?;
    if length > MESSAGE_LIMIT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; length];
    transfer(
        bytes.len(),
        |offset| stream.read(&mut bytes[offset..]),
        deadline,
        cancel,
    )?;
    Ok(bytes)
}

#[allow(
    clippy::too_many_arguments,
    reason = "One private handoff transfers four held capabilities and one bounded channel message."
)]
pub(crate) fn spawn(
    writer_directory: &Directory,
    attempt_directory: &Directory,
    writer: &LockGuard,
    custody: &LockGuard,
    environment: &[(OsString, OsString)],
    config_environment: &[OsString],
    payload: &[u8],
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<()> {
    if payload.len() > MESSAGE_LIMIT {
        return Err(CustodyError::InvalidRecord);
    }
    super::raw_exec::verify_child_wait_policy()?;
    let reaper = super::reaper::Reaper::new(1)?;
    let permit = reaper.reserve().ok_or(CustodyError::HandoffUnknown)?;
    let (mut parent, child_channel) = UnixStream::pair()?;
    let descriptors = [
        child_channel.as_fd(),
        writer_directory.handoff_file().as_fd(),
        attempt_directory.handoff_file().as_fd(),
        writer.handoff_file().as_fd(),
        custody.handoff_file().as_fd(),
    ];
    let executable = std::env::current_exe()?;
    let mut command = Command::new(executable);
    command.arg("internal").arg("supervise");
    for fd in &descriptors {
        command.arg(fd.as_raw_fd().to_string());
    }
    let guardian_environment: Vec<_> = environment
        .iter()
        .filter(|(name, _)| config_environment.contains(name))
        .cloned()
        .collect();
    command
        .env_clear()
        .envs(guardian_environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    super::child::configure_command(
        &mut command,
        &descriptors,
        Some(&guardian_environment),
        true,
    )?;
    let child = command.spawn()?;
    // The bounded detached reaper owns this direct guardian child on every path.
    // It never signals, so handoff failure cannot kill a healthy provider.
    permit.transfer(child);
    drop(child_channel);
    let end = deadline(timeout)?;
    send(
        &mut parent,
        payload,
        end.saturating_duration_since(Instant::now()),
        cancel,
    )
    .map_err(|_| CustodyError::HandoffUnknown)?;
    let remaining = end.saturating_duration_since(Instant::now());
    let response = receive_cancellable(&mut parent, remaining, cancel)
        .map_err(|_| CustodyError::HandoffUnknown)?;
    if response == b"custody-accepted-v1" {
        Ok(())
    } else {
        Err(CustodyError::HandoffUnknown)
    }
}

/// Adopt exactly the descriptors inherited by the private spawn above. Invalid
/// descriptors or an unlocked/mismatched lock refuse the handshake before exec.
pub(crate) fn inherit(args: &[OsString]) -> Result<Inherited> {
    let mut descriptors = Vec::with_capacity(args.len());
    let mut malformed = args.len() != 5;
    for arg in args {
        let Some(fd) = arg
            .to_str()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|fd| *fd > 2)
        else {
            malformed = true;
            continue;
        };
        if descriptors.contains(&fd) {
            malformed = true;
            continue;
        }
        descriptors.push(fd);
    }

    // Validate and adopt every unique live descriptor before socket or lock
    // checks. If any input is bad, successfully adopted descriptors close as
    // this vector drops; duplicate numbers are adopted only once.
    let mut owned = Vec::with_capacity(descriptors.len());
    let mut descriptor_error = None;
    for fd in descriptors {
        // SAFETY: F_GETFD receives an integral descriptor only and borrows no
        // memory. The fresh exec'd guardian has no concurrent fd mutator.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            if descriptor_error.is_none() {
                descriptor_error = Some(io::Error::last_os_error());
            }
            continue;
        }
        // SAFETY: F_GETFD just succeeded for this descriptor; numeric duplicates
        // were removed, and no Rust owner or concurrent fd mutator exists in this
        // fresh guardian. OwnedFd becomes its sole owner and closes it exactly once.
        owned.push((unsafe { OwnedFd::from_raw_fd(fd) }, flags));
    }
    if malformed {
        return Err(CustodyError::InvalidRecord);
    }
    if let Some(error) = descriptor_error {
        return Err(error.into());
    }
    for (descriptor, flags) in &owned {
        // SAFETY: `descriptor` owns a validated live fd. F_SETFD borrows no
        // memory; if it fails, all descriptors are still RAII-owned here.
        if unsafe {
            libc::fcntl(
                descriptor.as_raw_fd(),
                libc::F_SETFD,
                flags | libc::FD_CLOEXEC,
            )
        } < 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }

    let mut raw = owned.into_iter();
    let (socket_owner, _) = raw.next().ok_or(CustodyError::InvalidRecord)?;
    let socket = socket_owner.as_raw_fd();
    let mut socket_type: libc::c_int = 0;
    let mut size = std::mem::size_of_val(&socket_type) as libc::socklen_t;
    // SAFETY: size and socket_type are live aligned writable buffers of the
    // required ABI size; socket is a live OwnedFd and getsockopt retains no
    // pointer or ownership. Errors refuse conversion while RAII closes all fds.
    let socket_type_result = unsafe {
        libc::getsockopt(
            socket,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut socket_type as *mut libc::c_int).cast(),
            &mut size,
        )
    };
    if socket_type_result != 0
        || socket_type != libc::SOCK_STREAM
        || usize::try_from(size).ok() != Some(std::mem::size_of::<libc::c_int>())
    {
        return Err(CustodyError::InvalidRecord);
    }

    let mut address = std::mem::MaybeUninit::<libc::sockaddr_storage>::zeroed();
    let mut address_length = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: address is aligned writable sockaddr_storage-sized memory and
    // address_length points to its live ABI-sized length. socket is a live owned
    // descriptor; getsockname retains neither pointer nor ownership.
    if unsafe { libc::getsockname(socket, address.as_mut_ptr().cast(), &mut address_length) } != 0 {
        return Err(CustodyError::InvalidRecord);
    }
    let Some(address_length) = usize::try_from(address_length).ok() else {
        return Err(CustodyError::InvalidRecord);
    };
    if address_length < std::mem::size_of::<libc::sa_family_t>()
        || address_length > std::mem::size_of::<libc::sockaddr_storage>()
    {
        return Err(CustodyError::InvalidRecord);
    }
    // SAFETY: storage was zero-initialised, getsockname succeeded, and the
    // returned length includes the family field; untouched trailing bytes remain
    // initialised. The live socket's family is therefore safe to inspect.
    let address = unsafe { address.assume_init() };
    if i32::from(address.ss_family) != libc::AF_UNIX {
        return Err(CustodyError::InvalidRecord);
    }

    // SAFETY: socket_owner is the sole owner of a verified AF_UNIX SOCK_STREAM
    // descriptor. Transfer directly into UnixStream; no operation can fail or
    // create another owner between relinquishing and adopting the raw fd.
    let channel = unsafe { UnixStream::from_raw_fd(socket_owner.into_raw_fd()) };
    let mut files = Vec::with_capacity(raw.len());
    for (owner, _) in raw {
        // SAFETY: each owner is a distinct validated live descriptor. Ownership
        // moves directly from OwnedFd into File exactly once; later validation
        // failures drop the File and the remaining RAII owners.
        files.push(unsafe { File::from_raw_fd(owner.into_raw_fd()) });
    }
    let mut files = files.into_iter();
    let writer_directory =
        Directory::from_handoff(files.next().ok_or(CustodyError::InvalidRecord)?)?;
    let attempt_directory =
        Directory::from_handoff(files.next().ok_or(CustodyError::InvalidRecord)?)?;
    let writer = verify_lock(
        &writer_directory,
        "writer.lock",
        files.next().ok_or(CustodyError::InvalidRecord)?,
    )?;
    let custody = verify_lock(
        &attempt_directory,
        "custody.lock",
        files.next().ok_or(CustodyError::InvalidRecord)?,
    )?;
    Ok(Inherited {
        channel,
        writer_directory,
        attempt_directory,
        writer,
        custody,
    })
}

fn verify_lock(directory: &Directory, name: &str, file: File) -> Result<LockGuard> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    let root = directory.metadata()?;
    let leaf = directory.metadata_at(OsStr::new(name), false)?;
    if root.kind != crate::fs::FileKind::Directory
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != super::fs::effective_uid()
        || metadata.mode() & 0o022 != 0
        || root.uid != metadata.uid()
        || root.mode & 0o022 != 0
        || root.device != metadata.dev()
        || leaf.device != metadata.dev()
        || leaf.inode != metadata.ino()
    {
        return Err(CustodyError::InvalidRecord);
    }
    // Acquiring on the inherited description succeeds only while that exact
    // open description holds exclusion (or was previously unlocked). Probe a
    // separate description too, proving that it now excludes every other owner.
    super::fs::try_flock(&file, crate::fs::LockMode::Exclusive)?;
    let probe = super::fs::open_lock(directory.handoff_file(), OsStr::new(name))?;
    match super::fs::try_flock(&probe, crate::fs::LockMode::Exclusive) {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            Ok(LockGuard::from_handoff(file))
        }
        _ => Err(CustodyError::InvalidRecord),
    }
}
