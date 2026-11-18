use crate::pid::{CwdObservation, PidError, ProcessSignal, ProcessState};
use std::io;
#[cfg(unix)]
use std::os::fd::AsRawFd;

/// Kernel supplied peer PID for a connected Unix stream. The process id is
/// only a selector; callers must resolve it to a current native identity and
/// validate the relationship they need before granting authority.
#[cfg(target_os = "linux")]
pub(crate) fn unix_peer_pid(stream: &std::os::unix::net::UnixStream) -> io::Result<i64> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
    // SAFETY: `credentials` and `length` are writable for the syscall, and the
    // stream owns a valid connected Unix descriptor for this call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of_val(&credentials) || credentials.pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unix peer credentials are invalid",
        ));
    }
    if credentials.uid != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unix peer belongs to another user",
        ));
    }
    Ok(i64::from(credentials.pid))
}

/// Darwin's LOCAL_PEERPID is supplied by the kernel at connect time. The
/// private relay directory already enforces the effective-user boundary.
#[cfg(target_os = "macos")]
pub(crate) fn unix_peer_pid(stream: &std::os::unix::net::UnixStream) -> io::Result<i64> {
    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of_val(&pid) as libc::socklen_t;
    // SAFETY: `pid` and `length` are writable for the syscall, and the stream
    // owns a valid connected Unix descriptor for this call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of_val(&pid) || pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unix peer process id is invalid",
        ));
    }
    Ok(i64::from(pid))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn unix_peer_pid(_stream: &std::os::unix::net::UnixStream) -> io::Result<i64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "kernel Unix peer process identity is unavailable",
    ))
}

const MAX_EINTR_RETRIES: u64 = 8;
const OCCUPANCY_ATTEMPTS: usize = 4;
const OCCUPANCY_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(80);

/// Bounded executable and descriptor occupancy. Two snapshots must agree on
/// every object identity before the result is complete; any failure keeps the
/// paths already obtained as positive evidence but cannot prove non-occupancy,
/// and the observation names the most serious gap.
pub(crate) fn occupancy(pid: i32) -> crate::pid::OccupancyObservation {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        if linux_holds_nothing(pid) {
            return crate::pid::OccupancyObservation {
                paths: Vec::new(),
                complete: true,
                gap: None,
            };
        }
        occupancy_with_retries(|| {
            let first = occupancy_snapshot(pid);
            let second = occupancy_snapshot(pid);
            merge_snapshots(first, second)
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        crate::pid::OccupancyObservation::default()
    }
}

/// The caller owns retry pacing and its deadline. Take one agreeing snapshot
/// pair, without the ordinary per-process backoff, polling between descriptors.
pub(crate) fn occupancy_while(
    pid: i32,
    keep_going: &dyn Fn() -> bool,
) -> crate::pid::OccupancyObservation {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if !keep_going() {
            return crate::pid::OccupancyObservation::default();
        }
        #[cfg(target_os = "linux")]
        if linux_holds_nothing(pid) {
            return crate::pid::OccupancyObservation {
                paths: Vec::new(),
                complete: keep_going(),
                gap: None,
            };
        }
        let first = occupancy_snapshot_while(pid, keep_going);
        let second = occupancy_snapshot_while(pid, keep_going);
        let mut observation = merge_snapshots(first, second);
        if !keep_going() {
            observation.complete = false;
            observation.gap = Some(crate::pid::OccupancyGap::Unobserved);
        }
        observation
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (pid, keep_going);
        crate::pid::OccupancyObservation::default()
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[test]
fn bounded_occupancy_stops_during_descriptor_scan() {
    let polls = std::cell::Cell::new(0);
    let observed = occupancy_while(std::process::id() as i32, &|| {
        polls.set(polls.get() + 1);
        polls.get() <= 2
    });
    assert!(!observed.complete);
    assert_eq!(observed.gap, Some(crate::pid::OccupancyGap::Unobserved));
}

/// A process that has just exec'd opens and closes descriptors while its
/// runtime starts. That is churn, not a failure: observe again a bounded
/// number of times. Every attempt still needs two agreeing snapshots, and a
/// table that never settles stays incomplete with the paths found so far.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn occupancy_with_retries(
    attempt: impl FnMut() -> crate::pid::OccupancyObservation,
) -> crate::pid::OccupancyObservation {
    occupancy_with_retries_and_backoff(attempt, std::thread::sleep)
}

pub(crate) fn occupancy_with_retries_while(
    mut attempt: impl FnMut() -> crate::pid::OccupancyObservation,
    keep_going: &dyn Fn() -> bool,
) -> crate::pid::OccupancyObservation {
    occupancy_with_retries_and_backoff(
        || {
            if keep_going() {
                attempt()
            } else {
                crate::pid::OccupancyObservation::default()
            }
        },
        |delay| {
            if keep_going() {
                std::thread::sleep(delay);
            }
        },
    )
}

fn occupancy_with_retries_and_backoff(
    mut attempt: impl FnMut() -> crate::pid::OccupancyObservation,
    mut backoff: impl FnMut(std::time::Duration),
) -> crate::pid::OccupancyObservation {
    use crate::pid::OccupancyGap;
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    let mut last = crate::pid::OccupancyObservation::default();
    for attempt_number in 0..OCCUPANCY_ATTEMPTS {
        last = attempt();
        for path in &last.paths {
            if !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        // A process in exit teardown can briefly report an unreadable table
        // before it is absent or a zombie; retrying lets the census see that.
        if !matches!(
            last.gap,
            Some(OccupancyGap::Churn | OccupancyGap::DescriptorsUnavailable)
        ) {
            break;
        }
        if attempt_number + 1 < OCCUPANCY_ATTEMPTS {
            // Three pauses cap the added time per churning process at 240 ms.
            backoff(OCCUPANCY_RETRY_BACKOFF);
        }
    }
    last.paths = paths;
    last
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn merge_snapshots(
    first: OccupancySnapshot,
    second: OccupancySnapshot,
) -> crate::pid::OccupancyObservation {
    use crate::pid::OccupancyGap;
    let stable = first.identities == second.identities;
    let gap = [
        first.gap,
        second.gap,
        (!stable).then_some(OccupancyGap::Churn),
    ]
    .into_iter()
    .flatten()
    .reduce(OccupancyGap::worse);
    let mut paths = first.paths;
    for path in second.paths {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    crate::pid::OccupancyObservation {
        paths,
        complete: gap.is_none(),
        gap,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
const OCCUPANCY_LIMIT: usize = 16_384;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Default)]
struct OccupancySnapshot {
    paths: Vec<std::path::PathBuf>,
    /// (descriptor or image label, device, inode, path bytes)
    identities: Vec<(i64, u64, u64, Vec<u8>)>,
    /// The most serious failure; None means every query succeeded.
    gap: Option<crate::pid::OccupancyGap>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl OccupancySnapshot {
    fn note(&mut self, gap: crate::pid::OccupancyGap) {
        self.gap = Some(self.gap.map_or(gap, |known| known.worse(gap)));
    }
}

#[cfg(any(target_os = "linux", test))]
const PF_KTHREAD: u64 = 0x0020_0000;

/// Field 9 of /proc/pid/stat, after the parenthesised command name.
#[cfg(any(target_os = "linux", test))]
fn linux_stat_flags(stat: &str) -> Option<u64> {
    let (_, tail) = stat.rsplit_once(')')?;
    tail.split_whitespace().nth(6)?.parse().ok()
}

#[cfg(any(target_os = "linux", test))]
fn linux_stat_kernel_thread(stat: &str) -> bool {
    linux_stat_flags(stat).is_some_and(|flags| flags & PF_KTHREAD != 0)
}

/// Zombies and kernel threads hold no file in any worktree.
#[cfg(any(target_os = "linux", test))]
fn linux_stat_holds_nothing(stat: &str) -> bool {
    linux_stat_kernel_thread(stat) || linux_stat_state(stat).is_ok_and(|s| s == ProcessState::Gone)
}

#[cfg(target_os = "linux")]
fn linux_holds_nothing(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .is_ok_and(|stat| linux_stat_holds_nothing(&stat))
}

pub(crate) fn kernel_thread(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| linux_stat_kernel_thread(&stat))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn occupancy_snapshot(pid: i32) -> OccupancySnapshot {
    occupancy_snapshot_while(pid, &|| true)
}

#[cfg(target_os = "linux")]
fn occupancy_snapshot_while(pid: i32, keep_going: &dyn Fn() -> bool) -> OccupancySnapshot {
    use crate::pid::OccupancyGap;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    const DELETED: &[u8] = b" (deleted)";
    let mut snapshot = OccupancySnapshot::default();
    if !keep_going() {
        snapshot.note(crate::pid::OccupancyGap::Unobserved);
        return snapshot;
    }
    let proc = std::path::PathBuf::from(format!("/proc/{pid}"));
    let record = |label: i64, link: &std::path::Path, snapshot: &mut OccupancySnapshot| {
        let target = std::fs::read_link(link).ok();
        let identity = std::fs::metadata(link).ok();
        let Some(target) = target else { return false };
        let bytes = target.as_os_str().as_bytes();
        if !bytes.starts_with(b"/") {
            // Kernel pseudo-objects hold no filesystem location.
            let pseudo = [&b"socket:["[..], b"pipe:[", b"anon_inode:", b"net:["]
                .iter()
                .any(|prefix| bytes.starts_with(prefix));
            if pseudo {
                snapshot.identities.push((label, 0, 0, bytes.to_vec()));
            }
            return pseudo;
        }
        let Some(identity) = identity else {
            return false;
        };
        snapshot
            .identities
            .push((label, identity.dev(), identity.ino(), bytes.to_vec()));
        // An unlinked object still names its former parent; keep that location
        // as positive evidence rather than guessing it cannot matter.
        let trimmed = bytes.strip_suffix(DELETED).unwrap_or(bytes);
        if !trimmed.starts_with(b"/memfd:") {
            snapshot
                .paths
                .push(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
                    trimmed,
                )));
        }
        true
    };
    let exe = proc.join("exe");
    let exe_absent =
        matches!(std::fs::read_link(&exe), Err(error) if error.kind() == io::ErrorKind::NotFound);
    let exe_known = record(-1, &exe, &mut snapshot);
    let listing = || -> Result<Vec<std::ffi::OsString>, OccupancyGap> {
        let mut names = Vec::new();
        for entry in
            std::fs::read_dir(proc.join("fd")).map_err(|_| OccupancyGap::DescriptorsUnavailable)?
        {
            if !keep_going() {
                return Err(OccupancyGap::Unobserved);
            }
            if names.len() == OCCUPANCY_LIMIT {
                return Err(OccupancyGap::LimitExceeded);
            }
            names.push(
                entry
                    .map_err(|_| OccupancyGap::DescriptorsUnavailable)?
                    .file_name(),
            );
        }
        names.sort();
        Ok(names)
    };
    match listing() {
        Ok(names) => {
            // An absent executable with an empty, readable table is a kernel
            // thread or an exited process: it holds nothing. Otherwise the
            // executable is genuinely unavailable.
            if !exe_known && !(exe_absent && names.is_empty()) {
                snapshot.note(OccupancyGap::ExecutableUnavailable);
            }
            for name in &names {
                if !keep_going() {
                    snapshot.note(OccupancyGap::Unobserved);
                    return snapshot;
                }
                let Some(fd) = name.to_str().and_then(|text| text.parse::<i64>().ok()) else {
                    snapshot.note(OccupancyGap::DescriptorsUnavailable);
                    continue;
                };
                if !record(fd, &proc.join("fd").join(name), &mut snapshot) {
                    snapshot.note(OccupancyGap::DescriptorsUnavailable);
                }
            }
            // Descriptors opened or closed during the walk make it unprovable.
            if listing().ok().as_ref() != Some(&names) {
                snapshot.note(OccupancyGap::Churn);
            }
        }
        Err(gap) => {
            if !exe_known {
                snapshot.note(OccupancyGap::ExecutableUnavailable);
            }
            snapshot.note(gap);
        }
    }
    snapshot
}

/// The least serious macOS descriptor failure: libproc refused the query.
#[cfg(any(target_os = "macos", test))]
fn darwin_query_gap(errno: Option<i32>) -> crate::pid::OccupancyGap {
    if errno == Some(libc::EPERM) {
        crate::pid::OccupancyGap::DescriptorsDenied
    } else {
        crate::pid::OccupancyGap::DescriptorsUnavailable
    }
}

/// Whether a PROC_PIDLISTFDS byte count is a usable table, else why not.
/// libproc also returns zero when the query fails (EPERM for protected
/// daemons), so zero is never an empty table. A full buffer may be truncated,
/// and a partial record is malformed.
#[cfg(any(target_os = "macos", test))]
fn fd_list_records(
    count: i32,
    size: i32,
    record: usize,
    errno: Option<i32>,
) -> Result<usize, crate::pid::OccupancyGap> {
    if count <= 0 {
        return Err(darwin_query_gap(errno));
    }
    if count >= size {
        return Err(crate::pid::OccupancyGap::LimitExceeded);
    }
    if !(count as usize).is_multiple_of(record) {
        return Err(crate::pid::OccupancyGap::DescriptorsUnavailable);
    }
    Ok(count as usize / record)
}

/// A descriptor-info query is complete only when it fills the record.
#[cfg(any(target_os = "macos", test))]
fn fd_info_gap(count: i32, size: i32, errno: Option<i32>) -> Option<crate::pid::OccupancyGap> {
    (count != size).then(|| darwin_query_gap(errno))
}

/// An executable that no longer exists belongs to no existing root; it is
/// judged by cwd and descriptors. Any other failure is an unavailable image.
#[cfg(any(target_os = "macos", test))]
fn darwin_image_gap(metadata: &io::Result<(u64, u64)>) -> Option<crate::pid::OccupancyGap> {
    match metadata {
        Ok(_) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => Some(crate::pid::OccupancyGap::ExecutableUnavailable),
    }
}

#[cfg(target_os = "macos")]
fn darwin_clear_errno() {
    // SAFETY: __error returns this thread's live errno slot.
    let errno_slot = unsafe { libc::__error() };
    // SAFETY: the slot is thread-local and live; writing zero ensures a failed
    // call cannot be blamed on a stale error.
    unsafe { *errno_slot = 0 };
}

#[cfg(target_os = "macos")]
fn occupancy_snapshot_while(pid: i32, keep_going: &dyn Fn() -> bool) -> OccupancySnapshot {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    // XNU sys/proc_info.h: PROX_FDTYPE_VNODE is the only descriptor kind with
    // a file location. These kinds carry none: ATALK, SOCKET, PSHM, PSEM,
    // KQUEUE, PIPE, FSEVENTS, NEXUS, CHANNEL and NETPOLICY.
    const FDTYPE_VNODE: u32 = libc::PROX_FDTYPE_VNODE as u32;
    const LOCATIONLESS: [u32; 10] = [
        libc::PROX_FDTYPE_ATALK as u32,
        libc::PROX_FDTYPE_SOCKET as u32,
        libc::PROX_FDTYPE_PSHM as u32,
        libc::PROX_FDTYPE_PSEM as u32,
        libc::PROX_FDTYPE_KQUEUE as u32,
        libc::PROX_FDTYPE_PIPE as u32,
        libc::PROX_FDTYPE_FSEVENTS as u32,
        libc::PROX_FDTYPE_NEXUS as u32,
        libc::PROX_FDTYPE_CHANNEL as u32,
        libc::PROX_FDTYPE_NETPOLICY as u32,
    ];
    #[repr(C)]
    struct FileInfo {
        open_flags: u32,
        status: u32,
        offset: i64,
        kind: i32,
        guard_flags: u32,
    }
    #[repr(C)]
    struct VnodePath {
        file: FileInfo,
        vnode: libc::vnode_info_path,
    }
    let list = || -> Result<Vec<(i32, u32)>, crate::pid::OccupancyGap> {
        let mut records = vec![0u64; OCCUPANCY_LIMIT];
        let size = i32::try_from(records.len() * std::mem::size_of::<u64>())
            .map_err(|_| crate::pid::OccupancyGap::LimitExceeded)?;
        darwin_clear_errno();
        // SAFETY: the aligned buffer has size writable bytes; libproc borrows it.
        let count = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                records.as_mut_ptr().cast(),
                size,
            )
        };
        let errno = io::Error::last_os_error().raw_os_error();
        let len = fd_list_records(count, size, std::mem::size_of::<libc::proc_fdinfo>(), errno)?;
        // SAFETY: full fd records are initialized by libproc and u64 alignment suffices.
        let entries = unsafe {
            std::slice::from_raw_parts(records.as_ptr().cast::<libc::proc_fdinfo>(), len)
        };
        let mut values: Vec<_> = entries
            .iter()
            .map(|entry| (entry.proc_fd, entry.proc_fdtype))
            .collect();
        values.sort_unstable();
        Ok(values)
    };
    let mut snapshot = OccupancySnapshot::default();
    if !keep_going() {
        snapshot.note(crate::pid::OccupancyGap::Unobserved);
        return snapshot;
    }
    match darwin_executable_lookup(pid) {
        Ok(path) => {
            let metadata = std::fs::metadata(&path).map(|meta| (meta.dev(), meta.ino()));
            if let Some(gap) = darwin_image_gap(&metadata) {
                snapshot.note(gap);
            }
            match metadata {
                Ok((dev, ino)) => {
                    snapshot
                        .identities
                        .push((-1, dev, ino, path.as_os_str().as_bytes().to_vec()));
                    snapshot.paths.push(path);
                }
                // Deleted: no existing root contains it, and the next snapshot
                // must agree that it is still gone.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    snapshot
                        .identities
                        .push((-1, 0, 0, path.as_os_str().as_bytes().to_vec()));
                }
                Err(_) => snapshot.paths.push(path),
            }
        }
        Err(errno) => {
            if let Some(gap) = darwin_pidpath_gap(errno) {
                snapshot.note(gap);
            }
            // Deleted: the next snapshot must agree that it is still gone.
            snapshot.identities.push((-1, 0, 0, Vec::new()));
        }
    }
    let before = match list() {
        Ok(before) => before,
        Err(gap) => {
            snapshot.note(gap);
            return snapshot;
        }
    };
    for (fd, kind) in &before {
        if !keep_going() {
            snapshot.note(crate::pid::OccupancyGap::Unobserved);
            return snapshot;
        }
        if *kind != FDTYPE_VNODE {
            // Sockets, pipes, kqueues and shared memory carry no file location.
            if !LOCATIONLESS.contains(kind) {
                snapshot.note(crate::pid::OccupancyGap::DescriptorsUnavailable);
            }
            snapshot
                .identities
                .push((i64::from(*fd), u64::from(*kind), 0, Vec::new()));
            continue;
        }
        let mut info = std::mem::MaybeUninit::<VnodePath>::zeroed();
        let size = std::mem::size_of::<VnodePath>() as i32;
        darwin_clear_errno();
        // SAFETY: info is ABI-sized writable storage, borrowed only for this call.
        let count = unsafe { libc::proc_pidfdinfo(pid, *fd, 2, info.as_mut_ptr().cast(), size) };
        let errno = io::Error::last_os_error().raw_os_error();
        if let Some(gap) = fd_info_gap(count, size, errno) {
            snapshot.note(gap);
            continue;
        }
        // SAFETY: the full record was initialized by libproc.
        let info = unsafe { info.assume_init() };
        let stat = info.vnode.vip_vi.vi_stat;
        let raw: Vec<_> = info.vnode.vip_path.into_iter().flatten().collect();
        let bytes: Vec<u8> = raw
            .iter()
            .map(|byte| *byte as u8)
            .take_while(|byte| *byte != 0)
            .collect();
        snapshot.identities.push((
            i64::from(*fd),
            u64::from(stat.vst_dev),
            stat.vst_ino,
            bytes.clone(),
        ));
        if bytes.is_empty() && stat.vst_nlink == 0 {
            // Unlinked object: it cannot be inside any tree being removed.
            continue;
        }
        match decode_darwin_path(pid, &raw) {
            Ok(path) => snapshot.paths.push(path),
            Err(_) => snapshot.note(crate::pid::OccupancyGap::DescriptorsUnavailable),
        }
    }
    // Descriptor numbers can be reused; the caller compares object identities.
    if list().ok().as_ref() != Some(&before) {
        snapshot.note(crate::pid::OccupancyGap::Churn);
    }
    snapshot
}

#[cfg(test)]
#[test]
fn zero_descriptor_listing_is_a_failed_query_not_an_empty_table() {
    use crate::pid::OccupancyGap::*;
    let record = 8;
    assert_eq!(
        fd_list_records(0, 1024, record, Some(0)),
        Err(DescriptorsUnavailable),
        "failure shows as 0"
    );
    assert_eq!(
        fd_list_records(0, 1024, record, Some(libc::EPERM)),
        Err(DescriptorsDenied)
    );
    assert_eq!(
        fd_list_records(-1, 1024, record, None),
        Err(DescriptorsUnavailable)
    );
    assert_eq!(
        fd_list_records(1024, 1024, record, None),
        Err(LimitExceeded)
    );
    assert_eq!(
        fd_list_records(20, 1024, record, None),
        Err(DescriptorsUnavailable)
    );
    assert_eq!(fd_list_records(24, 1024, record, None), Ok(3));
}

#[cfg(test)]
#[test]
fn only_a_refused_descriptor_query_is_the_excusable_gap() {
    use crate::pid::OccupancyGap::*;
    assert_eq!(fd_info_gap(10, 10, None), None);
    assert_eq!(
        fd_info_gap(0, 10, Some(libc::EPERM)),
        Some(DescriptorsDenied)
    );
    assert_eq!(
        fd_info_gap(-1, 10, Some(libc::ESRCH)),
        Some(DescriptorsUnavailable)
    );
    assert_eq!(DescriptorsDenied.worse(Churn), Churn);
    assert_eq!(Churn.worse(DescriptorsDenied), Churn);
    assert_eq!(
        DescriptorsDenied.worse(DescriptorsDenied),
        DescriptorsDenied
    );
}

#[cfg(test)]
#[test]
fn startup_churn_is_retried_but_a_table_that_never_settles_stays_incomplete() {
    use crate::pid::{OccupancyGap, OccupancyObservation};
    let observation = |paths: &[&str], gap: Option<OccupancyGap>| OccupancyObservation {
        paths: paths.iter().map(std::path::PathBuf::from).collect(),
        complete: gap.is_none(),
        gap,
    };
    #[derive(Clone, Copy)]
    enum Scenario {
        SettlesAfterChurn,
        NeverSettles,
        ImmediateDenial,
        RecoversAfterUnavailable,
    }
    let rows = [
        (Scenario::SettlesAfterChurn, true, None, 2, 2),
        (
            Scenario::NeverSettles,
            false,
            Some(OccupancyGap::Churn),
            OCCUPANCY_ATTEMPTS,
            1,
        ),
        (
            Scenario::ImmediateDenial,
            false,
            Some(OccupancyGap::DescriptorsDenied),
            1,
            0,
        ),
        (Scenario::RecoversAfterUnavailable, true, None, 2, 1),
    ];
    for (scenario, complete, gap, expected_calls, retained_paths) in rows {
        let mut calls = 0;
        let mut backoffs = Vec::new();
        let result = occupancy_with_retries_and_backoff(
            || {
                calls += 1;
                match scenario {
                    Scenario::SettlesAfterChurn if calls == 1 => {
                        observation(&["/a", "/transient"], Some(OccupancyGap::Churn))
                    }
                    Scenario::NeverSettles => observation(&["/a"], Some(OccupancyGap::Churn)),
                    Scenario::ImmediateDenial => {
                        observation(&[], Some(OccupancyGap::DescriptorsDenied))
                    }
                    Scenario::RecoversAfterUnavailable if calls == 1 => {
                        observation(&[], Some(OccupancyGap::DescriptorsUnavailable))
                    }
                    _ => observation(&["/a"], None),
                }
            },
            |delay| backoffs.push(delay),
        );
        assert_eq!(calls, expected_calls);
        // Every retry, and only a retry, is preceded by one backoff.
        assert_eq!(backoffs.len(), calls - 1);
        assert!(
            backoffs
                .iter()
                .all(|delay| *delay == OCCUPANCY_RETRY_BACKOFF)
        );
        assert_eq!(result.complete, complete);
        assert_eq!(result.gap, gap);
        assert_eq!(result.paths.len(), retained_paths);
    }
}

#[cfg(test)]
#[test]
fn budget_expiry_stops_retries_and_retains_positive_paths() {
    use crate::pid::{OccupancyGap, OccupancyObservation};
    let calls = std::cell::Cell::new(0);
    let observed = occupancy_with_retries_while(
        || {
            calls.set(calls.get() + 1);
            OccupancyObservation {
                paths: vec!["/held".into()],
                complete: false,
                gap: Some(OccupancyGap::Churn),
            }
        },
        &|| calls.get() == 0,
    );
    assert_eq!(calls.get(), 1);
    assert!(!observed.complete);
    assert_eq!(observed.paths, vec![std::path::PathBuf::from("/held")]);
}

#[cfg(test)]
#[test]
fn deleted_executables_are_not_gaps_but_other_failures_are() {
    for errno in [
        Some(libc::ENOENT),
        Some(libc::EPERM),
        Some(libc::ESRCH),
        None,
    ] {
        assert_eq!(
            darwin_pidpath_gap(errno),
            (errno != Some(libc::ENOENT))
                .then_some(crate::pid::OccupancyGap::ExecutableUnavailable)
        );
    }
    let gone: io::Result<(u64, u64)> = Err(io::Error::from_raw_os_error(libc::ENOENT));
    assert_eq!(darwin_image_gap(&gone), None);
    assert_eq!(darwin_image_gap(&Ok((1, 2))), None);
    for errno in [libc::EPERM, libc::ESRCH, libc::EACCES] {
        let failed: io::Result<(u64, u64)> = Err(io::Error::from_raw_os_error(errno));
        assert_eq!(
            darwin_image_gap(&failed),
            Some(crate::pid::OccupancyGap::ExecutableUnavailable)
        );
    }
}

#[cfg(test)]
#[test]
fn linux_zombies_and_kernel_threads_hold_nothing() {
    let zombie = "123 (defunct) Z 1 123 123 0 -1 4194308 0 0 0 0 0 0 0 0 20 0 1 0 1";
    let kthread = "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 0 0 0 20 0 1 0 1";
    let user = "200 (a b) S 1 200 200 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 1";
    assert!(linux_stat_holds_nothing(zombie));
    assert!(linux_stat_holds_nothing(kthread));
    assert!(linux_stat_kernel_thread(kthread));
    assert!(!linux_stat_kernel_thread(zombie));
    assert!(!linux_stat_holds_nothing(user));
    assert!(!linux_stat_holds_nothing("garbage"));
}

#[cfg(test)]
fn test_uids(uid: u32) -> crate::pid::ProcessUids {
    crate::pid::ProcessUids {
        real: uid,
        effective: uid,
        saved: uid,
    }
}

#[cfg(any(target_os = "macos", test))]
fn darwin_thread_state(states: &[Option<i32>]) -> io::Result<ProcessState> {
    // These values are from Darwin's mach/thread_info.h; 5 is HALTED.
    // Missing a stall falls back to recovery; a false stall blocks recovery.
    const RUNNING: i32 = 1;
    const UNINTERRUPTIBLE: i32 = 4;
    if !states.iter().any(Option::is_some)
        || states
            .iter()
            .flatten()
            .any(|state| !(1..=5).contains(state))
    {
        return Err(io::Error::other("invalid thread states"));
    }
    Ok(
        if states.contains(&Some(UNINTERRUPTIBLE)) && !states.contains(&Some(RUNNING)) {
            ProcessState::Uninterruptible
        } else {
            ProcessState::Running
        },
    )
}

#[cfg(target_os = "macos")]
fn darwin_threads(pid: i32) -> io::Result<Vec<u64>> {
    // A full buffer may have been truncated. Do not infer a stall from an incomplete census.
    const MAX_THREADS: usize = 16_384;
    const PROC_PIDLISTTHREADS: i32 = 6;
    let mut threads = vec![0u64; MAX_THREADS];
    let size = i32::try_from(threads.len() * std::mem::size_of::<u64>())
        .map_err(|_| io::Error::other("thread buffer too large"))?;
    // SAFETY: the vector supplies writable storage of size bytes, retained by the caller.
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDLISTTHREADS,
            0,
            threads.as_mut_ptr().cast(),
            size,
        )
    };
    if count <= 0 || count >= size || !(count as usize).is_multiple_of(std::mem::size_of::<u64>()) {
        return Err(io::Error::other("thread list unavailable or incomplete"));
    }
    threads.truncate(count as usize / std::mem::size_of::<u64>());
    Ok(threads)
}

#[cfg(target_os = "macos")]
fn darwin_thread_states(pid: i32) -> io::Result<ProcessState> {
    let threads = darwin_threads(pid)?;
    let mut states = Vec::with_capacity(threads.len());
    let mut refreshed = None;
    let requested = std::mem::size_of::<libc::proc_threadinfo>() as i32;
    for thread in threads {
        let mut info = std::mem::MaybeUninit::<libc::proc_threadinfo>::uninit();
        // SAFETY: info supplies writable ABI-sized storage and libproc does not retain it.
        let returned = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTHREADINFO,
                thread,
                info.as_mut_ptr().cast(),
                requested,
            )
        };
        if returned != requested {
            // Ignore a failed query only if a fresh census proves the thread vanished.
            if refreshed.is_none() {
                refreshed = Some(darwin_threads(pid)?);
            }
            if refreshed
                .as_ref()
                .is_some_and(|current| current.contains(&thread))
            {
                return Err(io::Error::other("thread state unavailable"));
            }
            states.push(None);
            continue;
        }
        // SAFETY: a full-size successful call initialised the record.
        states.push(Some(unsafe { info.assume_init() }.pth_run_state));
    }
    darwin_thread_state(&states)
}

#[cfg(any(target_os = "linux", test))]
fn linux_stat_state(stat: &str) -> io::Result<ProcessState> {
    // comm is parenthesised and may itself contain spaces or parentheses.
    let (_, tail) = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid proc stat"))?;
    let state = tail
        .strip_prefix(' ')
        .and_then(|tail| tail.split_whitespace().next())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing proc state"))?;
    match state {
        "D" => Ok(ProcessState::Uninterruptible),
        "T" | "t" => Ok(ProcessState::Stopped),
        "Z" | "X" | "x" => Ok(ProcessState::Gone),
        "R" | "S" | "I" | "W" | "P" => Ok(ProcessState::Running),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown proc state",
        )),
    }
}

pub(crate) fn process_state(pid: i32) -> io::Result<ProcessState> {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => linux_stat_state(&stat),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ProcessState::Gone),
            Err(error) => Err(error),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let requested = std::mem::size_of::<libc::proc_bsdshortinfo>() as i32;
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
        // SAFETY: info is writable storage of the requested ABI size; libproc retains no pointer.
        let returned = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                requested,
            )
        };
        if returned == 0 {
            return Err(io::Error::other("process status unavailable"));
        }
        if returned != requested {
            return Err(io::Error::other("short process status record"));
        }
        // SAFETY: a full-size successful call initialised the record.
        let info = unsafe { info.assume_init() };
        if info.pbsi_status == libc::SZOMB {
            return Ok(ProcessState::Gone);
        }
        let threads = darwin_thread_states(pid)?;
        Ok(if threads == ProcessState::Uninterruptible {
            threads
        } else if info.pbsi_status == libc::SSTOP {
            ProcessState::Stopped
        } else {
            threads
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process state unsupported",
        ))
    }
}

#[cfg(test)]
#[test]
fn classifies_darwin_thread_states() {
    assert_eq!(
        darwin_thread_state(&[Some(4), Some(3)]).unwrap(),
        ProcessState::Uninterruptible
    );
    assert_eq!(
        darwin_thread_state(&[Some(4), Some(1)]).unwrap(),
        ProcessState::Running
    );
    assert_eq!(
        darwin_thread_state(&[Some(5), Some(3)]).unwrap(),
        ProcessState::Running
    );
    assert_eq!(
        darwin_thread_state(&[None, Some(4), Some(3)]).unwrap(),
        ProcessState::Uninterruptible
    );
    assert_eq!(
        darwin_thread_state(&[None, Some(4), Some(1)]).unwrap(),
        ProcessState::Running
    );
    assert!(darwin_thread_state(&[]).is_err());
    assert!(darwin_thread_state(&[None, None]).is_err());
    assert!(darwin_thread_state(&[Some(4), Some(99)]).is_err());
}

#[cfg(test)]
#[test]
fn parses_linux_stat_states_with_complex_names() {
    for (state, expected) in [
        ("D", ProcessState::Uninterruptible),
        ("T", ProcessState::Stopped),
        ("t", ProcessState::Stopped),
        ("S", ProcessState::Running),
        ("R", ProcessState::Running),
        ("Z", ProcessState::Gone),
    ] {
        assert_eq!(
            linux_stat_state(&format!("123 (name ) with spaces) {state} 1 2 3")).unwrap(),
            expected
        );
    }
    assert!(linux_stat_state("123 (broken").is_err());
    assert!(linux_stat_state("123 (name) ? 1").is_err());
}

pub(crate) fn process_uids(pid: i32) -> Option<crate::pid::ProcessUids> {
    #[cfg(target_os = "macos")]
    {
        darwin_uids(pid)
    }
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let mut fields = status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))?
            .split_whitespace();
        Some(crate::pid::ProcessUids {
            real: fields.next()?.parse().ok()?,
            effective: fields.next()?.parse().ok()?,
            saved: fields.next()?.parse().ok()?,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

pub(crate) fn unreadable_policy_uids(
    pid: i32,
    entries: &[crate::pid::CensusIgnoreEntry],
) -> Result<crate::pid::UnreadablePolicyExcusal, crate::pid::UnreadablePolicyFailure> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let before = darwin_uids(pid).ok_or("real/effective/saved uids unavailable")?;
        if !before.all_system() {
            return Err("real/effective/saved system uids below 500 required".into());
        }
        let path = darwin_executable_path(pid).ok_or("kernel executable path unavailable")?;
        let flags = darwin_signing_status(pid).ok_or("live signing state unavailable")?;
        if !darwin_valid_signature(flags) {
            return Err("valid signed image without untrusted flags required".into());
        }
        let executable = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|_| "O_NOFOLLOW executable open failed")?;
        if !executable
            .metadata()
            .map_err(|_| "executable metadata unavailable")?
            .is_file()
        {
            return Err("regular executable file required".into());
        }
        let builtin = darwin_builtin_condition(before, flags, &path, || {
            darwin_on_sealed_system_volume(&executable)
        });
        let team = if entries
            .iter()
            .any(|entry| entry.path.as_os_str() == path.as_os_str() && entry.team_id != "platform")
        {
            darwin_team_id(pid)
        } else {
            None
        };
        let platform_chain = || root_owned_executable_chain(&path, &executable);
        let listed = listed_image_matches(entries, &path, flags, team.as_deref(), platform_chain);
        if !listed {
            builtin.map_err(builtin_failure)?;
        }
        if darwin_executable_path(pid)
            .ok_or("executable path recheck unavailable")?
            .as_os_str()
            != path.as_os_str()
        {
            return Err("stable executable path required".into());
        }
        if darwin_signing_status(pid).ok_or("signing state recheck unavailable")? != flags {
            return Err("stable signing state required".into());
        }
        if darwin_uids(pid).ok_or("real/effective/saved uid recheck unavailable")? != before {
            return Err("stable real/effective/saved uids required".into());
        }
        if listed && team.is_some() && darwin_team_id(pid) != team {
            return Err(crate::pid::UnreadablePolicyFailure {
                condition: "stable signing team required",
                builtin_condition: builtin.err().map(BuiltinCondition::detail),
                configurable: false,
            });
        }
        Ok(crate::pid::UnreadablePolicyExcusal { uids: before })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, entries);
        Err("macOS signing evidence unavailable".into())
    }
}

/// A built-in refusal. Only an approved-directory or volume refusal of a
/// validly signed system process is one that a configured entry could excuse.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BuiltinCondition {
    SystemUids,
    PlatformSignature,
    ApprovedDaemonDirectory,
    SealedReadOnlyVolume,
    SealedVolumeEvidence,
}

#[cfg(any(target_os = "macos", test))]
impl BuiltinCondition {
    #[cfg(test)]
    const ALL: [Self; 5] = [
        Self::SystemUids,
        Self::PlatformSignature,
        Self::ApprovedDaemonDirectory,
        Self::SealedReadOnlyVolume,
        Self::SealedVolumeEvidence,
    ];

    fn detail(self) -> &'static str {
        match self {
            Self::SystemUids => "real/effective/saved system uids below 500 required",
            Self::PlatformSignature => "valid signed platform binary required",
            Self::ApprovedDaemonDirectory => "approved daemon directory required",
            Self::SealedReadOnlyVolume => "sealed read-only root volume required",
            Self::SealedVolumeEvidence => "sealed read-only root volume evidence unavailable",
        }
    }

    fn configurable(self) -> bool {
        match self {
            Self::SystemUids | Self::PlatformSignature => false,
            Self::ApprovedDaemonDirectory
            | Self::SealedReadOnlyVolume
            | Self::SealedVolumeEvidence => true,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn builtin_failure(condition: BuiltinCondition) -> crate::pid::UnreadablePolicyFailure {
    crate::pid::UnreadablePolicyFailure {
        condition: condition.detail(),
        builtin_condition: Some(condition.detail()),
        configurable: condition.configurable(),
    }
}

/// Directories of the sealed system volume. An executable here is still only
/// trusted after the root-owned chain and volume seal checks.
#[cfg(any(target_os = "macos", test))]
fn sealed_system_path(path: &std::path::Path) -> bool {
    path.components().all(|part| {
        matches!(
            part,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        )
    }) && [
        "/System",
        "/usr/libexec",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
    ]
    .iter()
    .any(|directory| path.starts_with(directory) && path != std::path::Path::new(directory))
}

/// A root-owned setuid image running with an effective uid of its owner but a
/// different real uid: the kernel refuses its cwd for the privilege change.
#[cfg(any(target_os = "macos", test))]
fn setuid_transition(mode: u32, owner: u32, uids: crate::pid::ProcessUids) -> bool {
    const SETUID: u32 = 0o4000;
    mode & SETUID != 0 && owner == 0 && uids.effective == owner && uids.real != uids.effective
}

/// The process image, when it is a root-owned executable on the sealed system
/// volume. Any missing evidence returns None.
pub(crate) fn sealed_system_image(pid: i32) -> Option<crate::pid::SealedSystemImage> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let path = darwin_executable_path(pid)?;
        if !sealed_system_path(&path) {
            return None;
        }
        let executable = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .ok()?;
        let metadata = executable.metadata().ok()?;
        if !root_owned_executable_chain(&path, &executable)
            || darwin_on_sealed_system_volume(&executable) != Some(true)
        {
            return None;
        }
        let uids = darwin_uids(pid)?;
        if darwin_executable_path(pid)? != path || darwin_uids(pid)? != uids {
            return None;
        }
        Some(crate::pid::SealedSystemImage {
            setuid_transition: setuid_transition(metadata.mode(), metadata.uid(), uids),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(test)]
#[test]
fn sealed_system_images_are_system_directories_and_setuid_needs_a_transition() {
    use std::path::Path;
    for path in [
        "/usr/bin/login",
        "/System/Library/CoreServices/x",
        "/usr/libexec/routined",
        "/usr/sbin/cfprefsd",
        "/sbin/launchd",
        "/bin/sh",
    ] {
        assert!(sealed_system_path(Path::new(path)), "{path}");
    }
    for path in [
        "/usr/bin",
        "/usr/local/bin/login",
        "/Users/me/usr/bin/login",
        "/usr/bin/../local/bin/x",
        "relative/usr/bin/x",
        "/Applications/Foo.app/x",
    ] {
        assert!(!sealed_system_path(Path::new(path)), "{path}");
    }
    let login = crate::pid::ProcessUids {
        real: 501,
        effective: 0,
        saved: 0,
    };
    assert!(setuid_transition(0o104555, 0, login));
    assert!(
        !setuid_transition(0o100555, 0, login),
        "needs the setuid bit"
    );
    assert!(!setuid_transition(0o104555, 501, login), "root owner only");
    assert!(
        !setuid_transition(0o104555, 0, test_uids(0)),
        "no transition"
    );
    assert!(!setuid_transition(0o104555, 0, test_uids(501)));
}

#[cfg(any(target_os = "macos", test))]
fn darwin_builtin_condition(
    uids: crate::pid::ProcessUids,
    flags: u32,
    path: &std::path::Path,
    sealed: impl FnOnce() -> Option<bool>,
) -> Result<(), BuiltinCondition> {
    if !uids.all_system() {
        return Err(BuiltinCondition::SystemUids);
    }
    if !darwin_platform_signature(flags) {
        return Err(BuiltinCondition::PlatformSignature);
    }
    if !darwin_daemon_path(path) {
        return Err(BuiltinCondition::ApprovedDaemonDirectory);
    }
    match sealed() {
        Some(true) => Ok(()),
        Some(false) => Err(BuiltinCondition::SealedReadOnlyVolume),
        None => Err(BuiltinCondition::SealedVolumeEvidence),
    }
}

#[cfg(test)]
#[test]
fn builtin_condition_names_each_failed_gate_and_accepts_complete_evidence() {
    use std::path::Path;
    let daemon = Path::new("/usr/libexec/daemon");
    let flags = CS_VALID | CS_SIGNED | CS_PLATFORM_BINARY;
    let cases = [
        (test_uids(0), flags, daemon, Some(true), Ok(())),
        (
            test_uids(500),
            flags,
            daemon,
            Some(true),
            Err(BuiltinCondition::SystemUids),
        ),
        (
            test_uids(0),
            CS_VALID | CS_SIGNED,
            daemon,
            Some(true),
            Err(BuiltinCondition::PlatformSignature),
        ),
        (
            test_uids(0),
            flags | CS_GET_TASK_ALLOW,
            daemon,
            Some(true),
            Err(BuiltinCondition::PlatformSignature),
        ),
        (
            test_uids(0),
            flags,
            Path::new("/usr/bin/login"),
            Some(true),
            Err(BuiltinCondition::ApprovedDaemonDirectory),
        ),
        (
            test_uids(0),
            flags,
            daemon,
            Some(false),
            Err(BuiltinCondition::SealedReadOnlyVolume),
        ),
        (
            test_uids(0),
            flags,
            daemon,
            None,
            Err(BuiltinCondition::SealedVolumeEvidence),
        ),
    ];
    for (uids, flags, path, sealed, expected) in cases {
        assert_eq!(
            darwin_builtin_condition(uids, flags, path, || sealed),
            expected
        );
    }
    for condition in BuiltinCondition::ALL {
        let failure = builtin_failure(condition);
        assert_eq!(failure.configurable, condition.configurable());
        // This exhaustive match is the compile-time guard for the ALL table.
        match condition {
            BuiltinCondition::SystemUids
            | BuiltinCondition::PlatformSignature
            | BuiltinCondition::ApprovedDaemonDirectory
            | BuiltinCondition::SealedReadOnlyVolume
            | BuiltinCondition::SealedVolumeEvidence => {}
        }
    }
    let plain: crate::pid::UnreadablePolicyFailure = "image policy evidence unavailable".into();
    assert!(!plain.configurable);
}

#[cfg(test)]
#[test]
fn allowed_daemon_on_unsealed_volume_reaches_seal_refusal() {
    let called = std::cell::Cell::new(false);
    let result = darwin_builtin_condition(
        crate::pid::ProcessUids {
            real: 0,
            effective: 88,
            saved: 0,
        },
        CS_VALID | CS_SIGNED | CS_PLATFORM_BINARY,
        std::path::Path::new("/System/Library/Daemon"),
        || {
            called.set(true);
            Some(false)
        },
    );
    assert!(called.get(), "all predicates before the seal check passed");
    assert_eq!(result, Err(BuiltinCondition::SealedReadOnlyVolume));
}

pub(crate) fn process_details(pid: i32) -> String {
    #[cfg(target_os = "macos")]
    {
        let path = darwin_executable_path(pid)
            .map_or_else(|| "unavailable".into(), |path| format!("{path:?}"));
        let uids = darwin_uids(pid).map_or_else(
            || "ruid=unavailable, euid=unavailable, svuid=unavailable".into(),
            |uids| {
                format!(
                    "ruid={}, euid={}, svuid={}",
                    uids.real, uids.effective, uids.saved
                )
            },
        );
        let status = darwin_signing_status(pid);
        let flags = status.map_or_else(|| "unavailable".into(), |flags| format!("0x{flags:08x}"));
        let team = if status.is_some_and(|flags| flags & CS_PLATFORM_BINARY != 0) {
            "platform".into()
        } else {
            darwin_team_id(pid).unwrap_or_else(|| "unavailable".into())
        };
        format!("path={path}, {uids}, cs_flags={flags}, team_id={team}")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        "path=unavailable, ruid=unavailable, euid=unavailable, svuid=unavailable, cs_flags=unavailable, team_id=unavailable".into()
    }
}

#[cfg(any(target_os = "macos", test))]
const CS_VALID: u32 = 0x0000_0001;
#[cfg(any(target_os = "macos", test))]
const CS_SIGNED: u32 = 0x2000_0000;
#[cfg(any(target_os = "macos", test))]
const CS_ADHOC: u32 = 0x0000_0002;
#[cfg(any(target_os = "macos", test))]
const CS_GET_TASK_ALLOW: u32 = 0x0000_0004;
#[cfg(any(target_os = "macos", test))]
const CS_RESTRICT: u32 = 0x0000_0020;
#[cfg(any(target_os = "macos", test))]
const CS_LINKER_SIGNED: u32 = 0x0002_0000;
#[cfg(any(target_os = "macos", test))]
const CS_PLATFORM_BINARY: u32 = 0x0400_0000;
#[cfg(test)]
const CS_PLATFORM_PATH: u32 = 0x0800_0000;
#[cfg(any(target_os = "macos", test))]
const CS_KILLED: u32 = 0x1000_0000;

#[cfg(any(target_os = "macos", test))]
fn darwin_valid_signature(flags: u32) -> bool {
    // CS_ADHOC may be absent on live ad-hoc images; this mask alone is not a
    // signing trust anchor. Platform bits or explicit path/team policy remain.
    const UNTRUSTED: u32 =
        CS_ADHOC | CS_GET_TASK_ALLOW | CS_RESTRICT | CS_LINKER_SIGNED | CS_KILLED;
    let required = CS_VALID | CS_SIGNED;
    flags & required == required && flags & UNTRUSTED == 0
}

#[cfg(any(target_os = "macos", test))]
fn darwin_platform_signature(flags: u32) -> bool {
    // CS_PLATFORM_PATH alone is not signing evidence.
    darwin_valid_signature(flags) && flags & CS_PLATFORM_BINARY != 0
}

#[cfg(any(target_os = "macos", test))]
fn listed_image_matches(
    entries: &[crate::pid::CensusIgnoreEntry],
    path: &std::path::Path,
    flags: u32,
    team: Option<&str>,
    platform_chain: impl Fn() -> bool,
) -> bool {
    darwin_valid_signature(flags)
        && entries.iter().any(|entry| {
            entry.is_valid()
                && entry.path.as_os_str() == path.as_os_str()
                && if entry.team_id == "platform" {
                    darwin_platform_signature(flags) && platform_chain()
                } else {
                    team == Some(entry.team_id.as_str())
                }
        })
}

#[cfg(any(target_os = "macos", test))]
fn root_owned_mode(uid: u32, mode: u32) -> bool {
    uid == 0 && mode & 0o022 == 0
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExecutableChainRefusal {
    ExecutableMetadata,
    ExecutableNotRootOwned,
    AbsolutePathRequired,
    RootDescriptorUnavailable,
    AncestorMetadata,
    AncestorNotRootOwned,
    NormalComponentRequired,
    AncestorOpen,
    ExecutableOpen,
    ExecutableMetadataUnavailable,
    DescriptorIdentityMismatch,
}

/// Walk from / using only no-follow descriptor-relative opens. The final
/// descriptor must identify the same file that was opened for image evidence.
#[cfg(target_os = "macos")]
fn root_owned_executable_chain(path: &std::path::Path, executable: &std::fs::File) -> bool {
    check_root_owned_executable_chain(path, executable).is_ok()
}

#[cfg(any(target_os = "macos", test))]
fn check_root_owned_executable_chain(
    path: &std::path::Path,
    executable: &std::fs::File,
) -> Result<(), ExecutableChainRefusal> {
    use std::os::unix::fs::MetadataExt;
    use std::path::Component;
    let expected = executable
        .metadata()
        .map_err(|_| ExecutableChainRefusal::ExecutableMetadata)?;
    if !expected.is_file() || !root_owned_mode(expected.uid(), expected.mode()) {
        return Err(ExecutableChainRefusal::ExecutableNotRootOwned);
    }
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return Err(ExecutableChainRefusal::AbsolutePathRequired);
    }
    let mut parts = parts.peekable();
    let mut directory = crate::sys::fs::open_anchor(std::path::Path::new("/"))
        .map_err(|_| ExecutableChainRefusal::RootDescriptorUnavailable)?;
    loop {
        let metadata = directory
            .metadata()
            .map_err(|_| ExecutableChainRefusal::AncestorMetadata)?;
        if !metadata.is_dir() || !root_owned_mode(metadata.uid(), metadata.mode()) {
            return Err(ExecutableChainRefusal::AncestorNotRootOwned);
        }
        let Some(Component::Normal(name)) = parts.next() else {
            return Err(ExecutableChainRefusal::NormalComponentRequired);
        };
        if parts.peek().is_some() {
            directory = crate::sys::fs::open_directory(&directory, name, false)
                .map_err(|_| ExecutableChainRefusal::AncestorOpen)?;
        } else {
            let file = crate::sys::fs::open_read(&directory, name)
                .map_err(|_| ExecutableChainRefusal::ExecutableOpen)?;
            let metadata = file
                .metadata()
                .map_err(|_| ExecutableChainRefusal::ExecutableMetadataUnavailable)?;
            return if metadata.is_file()
                && root_owned_mode(metadata.uid(), metadata.mode())
                && (metadata.dev(), metadata.ino()) == (expected.dev(), expected.ino())
            {
                Ok(())
            } else {
                Err(ExecutableChainRefusal::DescriptorIdentityMismatch)
            };
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn darwin_daemon_path(path: &std::path::Path) -> bool {
    path.components().all(|part| {
        matches!(
            part,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        )
    }) && DARWIN_DAEMON_DIRECTORIES
        .iter()
        .any(|directory| path.starts_with(directory) && path != std::path::Path::new(directory))
}

#[cfg(any(target_os = "macos", test))]
const DARWIN_DAEMON_DIRECTORIES: [&str; 4] =
    ["/sbin", "/usr/sbin", "/usr/libexec", "/System/Library"];

#[cfg(target_os = "macos")]
fn darwin_executable_lookup(pid: i32) -> Result<std::path::PathBuf, Option<i32>> {
    use std::os::unix::ffi::OsStrExt;
    let mut path = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    darwin_clear_errno();
    // SAFETY: path supplies writable storage of the supplied size, not retained.
    let size = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    if size <= 0 || size as usize >= path.len() {
        return Err(io::Error::last_os_error().raw_os_error());
    }
    let end = path.iter().position(|byte| *byte == 0).ok_or(None)?;
    let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&path[..end]));
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(None)
    }
}

#[cfg(target_os = "macos")]
fn darwin_executable_path(pid: i32) -> Option<std::path::PathBuf> {
    darwin_executable_lookup(pid).ok()
}

pub(crate) fn executable_path(pid: i32) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        darwin_executable_path(pid)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// proc_pidpath reports ENOENT for a process whose executable file has been
/// deleted (macOS has no "(deleted)" marker). Like a deleted Linux image it
/// belongs to no existing root and is judged by cwd and descriptors.
#[cfg(any(target_os = "macos", test))]
fn darwin_pidpath_gap(errno: Option<i32>) -> Option<crate::pid::OccupancyGap> {
    (errno != Some(libc::ENOENT)).then_some(crate::pid::OccupancyGap::ExecutableUnavailable)
}

#[cfg(any(target_os = "macos", test))]
fn darwin_sealed_root_volume(flags: u32, valid: u32, capabilities: u32) -> bool {
    // XNU bsd/sys/{mount,attr}.h: read-only root filesystem and a valid seal.
    const REQUIRED_MOUNT: u32 = 0x0000_0001 | 0x0000_4000;
    const SEALED: u32 = 0x0200_0000;
    flags & REQUIRED_MOUNT == REQUIRED_MOUNT && valid & SEALED != 0 && capabilities & SEALED != 0
}

#[cfg(target_os = "macos")]
fn darwin_csops(pid: i32, operation: u32, buffer: &mut [u8]) -> bool {
    unsafe extern "C" {
        fn csops(
            pid: libc::pid_t,
            ops: libc::c_uint,
            buffer: *mut libc::c_void,
            size: usize,
        ) -> libc::c_int;
    }
    // SAFETY: the supplied slice is live writable storage of the declared
    // length. csops copies bytes to userspace and retains no pointer.
    let result = unsafe { csops(pid, operation, buffer.as_mut_ptr().cast(), buffer.len()) };
    result == 0
}

#[cfg(target_os = "macos")]
fn darwin_signing_status(pid: i32) -> Option<u32> {
    let mut flags = [0u8; 4];
    darwin_csops(pid, 0, &mut flags).then(|| u32::from_ne_bytes(flags))
}

#[cfg(target_os = "macos")]
fn darwin_team_id(pid: i32) -> Option<String> {
    const CS_OPS_TEAMID: u32 = 14;
    let mut bytes = [0xff; 8 + 64 + 1];
    if !darwin_csops(pid, CS_OPS_TEAMID, &mut bytes) {
        return None;
    }
    decode_darwin_team_id(&bytes).map(str::to_owned)
}

#[cfg(any(target_os = "macos", test))]
fn decode_darwin_team_id(bytes: &[u8]) -> Option<&str> {
    if bytes.get(..4)? != [0; 4] {
        return None;
    }
    let length = u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
    if !(10..=8 + 64 + 1).contains(&length) {
        return None;
    }
    let payload = bytes.get(8..length)?;
    let (&0, team) = payload.split_last()? else {
        return None;
    };
    if !team.iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    std::str::from_utf8(team).ok()
}

#[cfg(target_os = "macos")]
fn darwin_uids(pid: i32) -> Option<crate::pid::ProcessUids> {
    // XNU proc_info.c exposes all three kernel uids in the short record without
    // CHECK_SAME_USER. Full TBSDINFO is restricted for precisely these daemons.
    // This is a complete credential read, never an effective-uid-only fallback.
    let requested = std::mem::size_of::<libc::proc_bsdshortinfo>() as i32;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::uninit();
    // SAFETY: aligned writable storage has the exact ABI size; no pointer is retained.
    let returned = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            requested,
        )
    };
    if returned != requested {
        return None;
    }
    // SAFETY: the exact-size successful read initialised the whole record.
    let info = unsafe { info.assume_init() };
    (info.pbsi_pid == pid as u32).then_some(crate::pid::ProcessUids {
        real: info.pbsi_ruid,
        effective: info.pbsi_uid,
        saved: info.pbsi_svuid,
    })
}

#[cfg(target_os = "macos")]
fn darwin_on_sealed_system_volume(executable: &std::fs::File) -> Option<bool> {
    use std::os::fd::AsRawFd;
    let mut volume = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: volume supplies aligned ABI-sized storage for this borrowed fd.
    if unsafe { libc::fstatfs(executable.as_raw_fd(), volume.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: fstatfs successfully initialised the record.
    let volume = unsafe { volume.assume_init() };
    let required = (libc::MNT_RDONLY | libc::MNT_ROOTFS) as u32;
    if volume.f_flags & required != required {
        return Some(false);
    }
    // Query the executable's mount point, supplied by fstatfs, rather than
    // guessing trust from its pathname (which may traverse a Data firmlink).
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    #[repr(C)]
    struct Capabilities {
        length: u32,
        capabilities: libc::vol_capabilities_attr_t,
    }
    let mut capabilities = Capabilities {
        length: 0,
        capabilities: libc::vol_capabilities_attr_t {
            capabilities: [0; 4],
            valid: [0; 4],
        },
    };
    // SAFETY: the kernel's mount-point array is NUL-terminated; attrs and
    // result are aligned ABI-sized storage and no pointers are retained.
    let result = unsafe {
        libc::getattrlist(
            volume.f_mntonname.as_ptr(),
            (&mut attrs as *mut libc::attrlist).cast(),
            (&mut capabilities as *mut Capabilities).cast(),
            std::mem::size_of::<Capabilities>(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    if capabilities.length as usize != std::mem::size_of::<Capabilities>() {
        return None;
    }
    let format = libc::VOL_CAPABILITIES_FORMAT;
    Some(darwin_sealed_root_volume(
        volume.f_flags,
        capabilities.capabilities.valid[format],
        capabilities.capabilities.capabilities[format],
    ))
}

pub(crate) fn signal_process(pid: i32, signal: ProcessSignal) -> io::Result<()> {
    // Keep group signalling impossible even if another internal caller is added.
    if pid <= 0 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let signal = match signal {
        ProcessSignal::Stop => libc::SIGSTOP,
        ProcessSignal::Continue => libc::SIGCONT,
    };
    // SAFETY: pid is positive so kill addresses only that process; signal is
    // one of the two valid constants above. No pointers or ownership transfer.
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn failure(pid: i32, source: io::Error) -> PidError {
    PidError::Io { pid, source }
}
pub(crate) fn cwd(pid: i32) -> Result<CwdObservation, PidError> {
    #[cfg(target_os = "linux")]
    {
        linux_cwd(pid)
    }
    #[cfg(target_os = "macos")]
    {
        darwin_cwd(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        Ok(CwdObservation::Unsupported)
    }
}

#[cfg(target_os = "linux")]
fn linux_cwd(pid: i32) -> Result<CwdObservation, PidError> {
    let mut retries = 0;
    loop {
        match std::fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(path) if path.is_absolute() => return Ok(CwdObservation::Path(path)),
            Ok(_) => {
                return Err(PidError::MalformedCwdPath {
                    pid,
                    reason: "path is not absolute",
                });
            }
            Err(error)
                if error.kind() == io::ErrorKind::Interrupted && retries < MAX_EINTR_RETRIES =>
            {
                retries += 1;
            }
            Err(error) => return map_cwd_error(pid, error),
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn map_cwd_error(pid: i32, error: io::Error) -> Result<CwdObservation, PidError> {
    match error.raw_os_error() {
        Some(libc::ESRCH | libc::ENOENT) => Ok(CwdObservation::Gone),
        Some(libc::EPERM | libc::EACCES) => Ok(CwdObservation::PermissionDenied),
        Some(errno) if errno == libc::ENOTSUP || errno == libc::ENOSYS => {
            Ok(CwdObservation::Unsupported)
        }
        _ => Err(failure(pid, error)),
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
enum DarwinCwdCall {
    Gone,
    PermissionDenied,
    Unsupported,
    Retry,
    Record,
}

/// The result of probing a process whose cwd libproc would not report.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DarwinLiveness {
    Alive,
    Gone,
    Unknown,
}

#[cfg(any(target_os = "macos", test))]
fn classify_darwin_result(
    pid: i32,
    size: i32,
    expected: usize,
    error: Option<io::Error>,
    liveness: impl FnOnce() -> DarwinLiveness,
) -> Result<DarwinCwdCall, PidError> {
    if size == 0 {
        // Zero alone proves neither absence nor EPERM. Only verified ESRCH
        // establishes disappearance; only the cwd syscall's EPERM can reach
        // the daemon policy. Production clears errno before the syscall.
        if liveness() == DarwinLiveness::Gone {
            return Ok(DarwinCwdCall::Gone);
        }
        return match error {
            Some(error) if error.raw_os_error() == Some(libc::EPERM) => {
                Ok(DarwinCwdCall::PermissionDenied)
            }
            Some(error) if error.raw_os_error().is_some_and(|errno| errno != 0) => {
                Err(failure(pid, error))
            }
            _ => Err(failure(pid, io::Error::from_raw_os_error(libc::EIO))),
        };
    }
    if size < 0 {
        let error = match error {
            Some(error) => error,
            None => io::Error::from_raw_os_error(libc::EIO),
        };
        match error.raw_os_error() {
            Some(libc::EINTR) => return Ok(DarwinCwdCall::Retry),
            Some(libc::ESRCH) => return Ok(DarwinCwdCall::Gone),
            Some(libc::EPERM) => return Ok(DarwinCwdCall::PermissionDenied),
            Some(errno)
                if errno == libc::ENOTSUP || errno == libc::EOPNOTSUPP || errno == libc::ENOSYS =>
            {
                return Ok(DarwinCwdCall::Unsupported);
            }
            _ => return Err(failure(pid, error)),
        }
    }
    let returned = size as usize;
    if returned != expected {
        return Err(PidError::CwdShortRecord {
            pid,
            returned,
            expected,
        });
    }
    Ok(DarwinCwdCall::Record)
}

#[cfg(any(target_os = "macos", test))]
fn decode_darwin_path(pid: i32, raw: &[libc::c_char]) -> Result<std::path::PathBuf, PidError> {
    use std::os::unix::ffi::OsStringExt;

    let bytes: Vec<u8> = raw.iter().map(|byte| *byte as u8).collect();
    let Some(end) = bytes.iter().position(|byte| *byte == 0) else {
        return Err(PidError::MalformedCwdPath {
            pid,
            reason: "path is not NUL terminated",
        });
    };
    let Some(path) = bytes.get(..end) else {
        return Err(PidError::MalformedCwdPath {
            pid,
            reason: "path range is invalid",
        });
    };
    if path.first() != Some(&b'/') {
        return Err(PidError::MalformedCwdPath {
            pid,
            reason: "path is empty or not absolute",
        });
    }
    Ok(std::ffi::OsString::from_vec(path.to_vec()).into())
}

#[cfg(target_os = "macos")]
fn darwin_liveness(pid: i32) -> DarwinLiveness {
    // Mirror the census rule: zero from libproc is ambiguous, so getsid decides
    // absence. Clear errno first because a failed getsid may inherit stale errno.
    // SAFETY: __error returns this thread's live errno slot; writing zero
    // ensures a failed getsid result cannot inherit stale errno.
    let errno_slot = unsafe { libc::__error() };
    // SAFETY: errno_slot is the live thread-local slot returned immediately
    // above; writing it cannot affect another thread.
    unsafe { *errno_slot = 0 };
    // SAFETY: getsid reads only the scalar PID, retains no pointer, and reports
    // failure through this thread's errno slot.
    let session = unsafe { libc::getsid(pid) };
    // SAFETY: __error returns the same live thread-local slot after the call.
    let errno_slot = unsafe { libc::__error() };
    // SAFETY: errno_slot is the live thread-local slot returned immediately
    // above; only its integer value is read.
    let errno = unsafe { *errno_slot };
    if session > 0 {
        DarwinLiveness::Alive
    } else if session == -1 && errno == libc::ESRCH {
        DarwinLiveness::Gone
    } else {
        // Another failure, including EPERM/EACCES, cannot prove absence.
        DarwinLiveness::Unknown
    }
}

#[cfg(target_os = "macos")]
fn darwin_cwd(pid: i32) -> Result<CwdObservation, PidError> {
    let expected = std::mem::size_of::<libc::proc_vnodepathinfo>();
    let mut retries = 0;
    loop {
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let buffer_size = i32::try_from(expected)
            .map_err(|_| failure(pid, io::Error::from_raw_os_error(libc::EOVERFLOW)))?;
        // SAFETY: __error returns this thread's live errno slot; clearing it
        // prevents a zero libproc result from inheriting stale EPERM.
        let errno_slot = unsafe { libc::__error() };
        // SAFETY: this is the live thread-local slot returned just above.
        unsafe { *errno_slot = 0 };
        // SAFETY: info is aligned writable storage of exactly the requested
        // vnodepathinfo size. proc_pidinfo only borrows it; full success writes
        // all fields. No handles transfer or global state is exposed.
        let size = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast(),
                buffer_size,
            )
        };
        let error = (size <= 0).then(io::Error::last_os_error);
        match classify_darwin_result(pid, size, expected, error, || darwin_liveness(pid))? {
            DarwinCwdCall::Gone => return Ok(CwdObservation::Gone),
            DarwinCwdCall::PermissionDenied => return Ok(CwdObservation::PermissionDenied),
            DarwinCwdCall::Unsupported => return Ok(CwdObservation::Unsupported),
            DarwinCwdCall::Retry if retries < MAX_EINTR_RETRIES => {
                retries += 1;
            }
            DarwinCwdCall::Retry => {
                return Err(failure(pid, io::Error::from(io::ErrorKind::Interrupted)));
            }
            DarwinCwdCall::Record => {
                // SAFETY: classification accepted only a full-size success,
                // which initialises the complete record.
                let info = unsafe { info.assume_init() };
                let raw: Vec<_> = info.pvi_cdir.vip_path.into_iter().flatten().collect();
                return decode_darwin_path(pid, &raw).map(CwdObservation::Path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unix_peer_pid_is_supplied_by_the_kernel() -> std::io::Result<()> {
        let (client, _server) = std::os::unix::net::UnixStream::pair()?;
        assert_eq!(
            crate::pid::unix_peer_pid(&client)?,
            i64::from(std::process::id())
        );
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    struct Reaped(std::process::Child);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn native_occupancy_reports_image_and_open_file_with_identity_completeness() {
        // A single-threaded child keeps its descriptor table still, so the
        // two identity snapshots can agree. spawn returns after exec succeeds.
        let file = tempfile::NamedTempFile::new().unwrap();
        let child = Reaped(
            std::process::Command::new("sleep")
                .arg("30")
                .stdin(std::fs::File::open(file.path()).unwrap())
                .spawn()
                .unwrap(),
        );
        // A fresh exec opens and closes descriptors while its runtime starts;
        // observe the settled process (asleep in sleep(1)).
        #[cfg(target_os = "linux")]
        provenant_test_support::wait_for(
            "settled process",
            std::time::Duration::from_secs(5),
            || {
                std::fs::read_to_string(format!("/proc/{}/stat", child.0.id()))
                    .ok()
                    .and_then(|stat| {
                        stat.rsplit_once(')')
                            .map(|(_, tail)| tail.trim_start().starts_with('S'))
                    })
                    == Some(true)
            },
            |asleep| *asleep,
        )
        .unwrap();
        #[cfg(target_os = "macos")]
        let observation = provenant_test_support::wait_for(
            "complete process occupancy",
            std::time::Duration::from_secs(5),
            || occupancy(child.0.id() as i32),
            |observation| observation.complete,
        )
        .unwrap();
        #[cfg(target_os = "linux")]
        let observation = occupancy(child.0.id() as i32);
        let paths: Vec<_> = observation
            .paths
            .iter()
            .filter_map(|path| std::fs::canonicalize(path).ok())
            .collect();
        assert!(
            paths.contains(&std::fs::canonicalize(file.path()).unwrap()),
            "{observation:?}"
        );
        assert!(
            paths
                .iter()
                .any(|path| path.file_name().is_some_and(|name| name == "sleep")),
            "{observation:?}"
        );
        assert!(observation.complete, "{observation:?}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn native_occupancy_never_proves_an_unqueryable_process_empty() {
        for pid in [0, -1, i32::MAX] {
            let observation = occupancy(pid);
            assert!(
                !observation.complete && observation.paths.is_empty(),
                "{pid}"
            );
        }
        // A reaped pid has no listing to prove empty.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        child.wait().unwrap();
        assert!(!occupancy(pid).complete);
    }

    use super::*;

    #[test]
    fn host_listing_requires_exact_path_signer_and_valid_live_signature() {
        use crate::pid::CensusIgnoreEntry;
        let entries = [CensusIgnoreEntry {
            path: "/Library/Daemon".into(),
            team_id: "TEAM123".into(),
        }];
        let path = std::path::Path::new("/Library/Daemon");
        let signed = CS_VALID | CS_SIGNED;
        assert!(
            listed_image_matches(&entries, path, signed, Some("TEAM123"), || true),
            "accepted listing"
        );
        assert!(
            !listed_image_matches(&entries, path, signed, Some("TEAM999"), || true),
            "team mismatch"
        );
        assert!(
            !listed_image_matches(
                &entries,
                std::path::Path::new("/Library/Daemon-child"),
                signed,
                Some("TEAM123"),
                || true
            ),
            "exact path mismatch"
        );
        assert!(
            !listed_image_matches(
                &entries,
                std::path::Path::new("/Library//Daemon"),
                signed,
                Some("TEAM123"),
                || true
            ),
            "do not normalize a listed path match"
        );
        assert!(
            !listed_image_matches(&entries, path, signed, None, || true),
            "unreadable team"
        );
        for invalid in [
            signed & !1,
            signed | 2,
            signed | 4,
            signed | CS_RESTRICT,
            signed | CS_LINKER_SIGNED,
            signed | CS_KILLED,
        ] {
            assert!(!listed_image_matches(
                &entries,
                path,
                invalid,
                Some("TEAM123"),
                || true
            ));
        }
        let platform = [CensusIgnoreEntry {
            path: path.into(),
            team_id: "platform".into(),
        }];
        assert!(!listed_image_matches(
            &platform,
            path,
            signed,
            Some("platform"),
            || true
        ));
        assert!(listed_image_matches(
            &platform,
            path,
            signed | CS_PLATFORM_BINARY,
            None,
            || true
        ));
        assert!(
            !listed_image_matches(&platform, path, signed | CS_PLATFORM_BINARY, None, || false),
            "platform selector requires the root-owned protected descriptor chain"
        );
        assert!(
            !listed_image_matches(
                &platform,
                path,
                signed | CS_PLATFORM_BINARY | CS_LINKER_SIGNED,
                None,
                || true
            ),
            "linker-signed code is refused even with a platform bit"
        );
    }

    #[test]
    fn platform_listing_requires_a_root_owned_nonwritable_descriptor_chain()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixtures = root_owned_executable_fixtures()?;
        let (path, file) = fixtures.first().ok_or("no protected executable fixture")?;
        let (different_path, different) = fixtures
            .get(1)
            .ok_or("second protected executable fixture unavailable")?;
        assert_eq!(check_root_owned_executable_chain(path, file), Ok(()));
        assert_eq!(
            check_root_owned_executable_chain(path, different),
            Err(ExecutableChainRefusal::DescriptorIdentityMismatch)
        );
        assert_ne!(path, different_path);

        let temp = tempfile::tempdir()?;
        let path = std::fs::canonicalize(temp.path())?.join("image");
        std::fs::write(&path, b"image")?;
        let file = crate::sys::fs::open_source(&path)?;
        assert_eq!(
            check_root_owned_executable_chain(&path, &file),
            // The user-owned executable is refused before its ancestors are walked.
            Err(ExecutableChainRefusal::ExecutableNotRootOwned)
        );
        Ok(())
    }

    fn root_owned_executable_fixtures() -> io::Result<Vec<(std::path::PathBuf, std::fs::File)>> {
        use std::os::unix::fs::MetadataExt;
        let mut fixtures: Vec<(std::path::PathBuf, std::fs::File)> = Vec::new();
        for directory in DARWIN_DAEMON_DIRECTORIES {
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(path) = std::fs::canonicalize(entry.path()) else {
                    continue;
                };
                if fixtures.iter().any(|(known, _)| known == &path) {
                    continue;
                }
                let Ok(file) = crate::sys::fs::open_source(&path) else {
                    continue;
                };
                let identity = file.metadata()?;
                if fixtures.iter().any(|(_, known)| {
                    known.metadata().is_ok_and(|metadata| {
                        (metadata.dev(), metadata.ino()) == (identity.dev(), identity.ino())
                    })
                }) {
                    continue;
                }
                if check_root_owned_executable_chain(&path, &file).is_ok() {
                    fixtures.push((path, file));
                    if fixtures.len() == 2 {
                        return Ok(fixtures);
                    }
                }
            }
        }
        Ok(fixtures)
    }

    #[test]
    fn team_id_blob_requires_bounded_network_length_and_exact_nul_terminated_identity() {
        let mut bytes = vec![0; 8];
        bytes.extend_from_slice(b"TEAM123\0");
        let length = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&length.to_be_bytes());
        assert_eq!(decode_darwin_team_id(&bytes), Some("TEAM123"));
        for length in [0u32, 9, 74, u32::MAX] {
            let mut malformed = bytes.clone();
            malformed[4..8].copy_from_slice(&length.to_be_bytes());
            assert_eq!(decode_darwin_team_id(&malformed), None);
        }
        for (index, value) in [(0, 1), (9, 0), (15, 1), (10, 0xff)] {
            let mut malformed = bytes.clone();
            malformed[index] = value;
            assert_eq!(decode_darwin_team_id(&malformed), None);
        }
        assert_eq!(decode_darwin_team_id(&bytes[..7]), None);
        bytes.extend_from_slice(b"IGNORED");
        assert_eq!(decode_darwin_team_id(&bytes), Some("TEAM123"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_current_process_is_not_an_eligible_platform_daemon()
    -> Result<(), Box<dyn std::error::Error>> {
        let pid = std::process::id() as i32;
        let uids = darwin_uids(pid).ok_or("complete kernel credential record unavailable")?;
        assert_eq!(uids.effective, crate::fs::effective_uid());
        assert!(unreadable_policy_uids(pid, &[]).is_err());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_same_user_sleep_is_not_an_eligible_daemon() -> Result<(), Box<dyn std::error::Error>>
    {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Child(std::process::Command::new("/bin/sleep").arg("30").spawn()?);
        assert!(child.0.try_wait()?.is_none());
        assert!(unreadable_policy_uids(child.0.id() as i32, &[]).is_err());
        Ok(())
    }

    #[test]
    fn platform_policy_requires_valid_signed_platform_code() {
        let required = CS_SIGNED | CS_PLATFORM_BINARY | CS_VALID;
        assert!(darwin_platform_signature(required));
        for missing in [CS_VALID, CS_PLATFORM_BINARY, CS_SIGNED] {
            assert!(!darwin_platform_signature(required & !missing));
        }
        for untrusted in [
            CS_ADHOC,
            CS_GET_TASK_ALLOW,
            CS_RESTRICT,
            CS_LINKER_SIGNED,
            CS_KILLED,
        ] {
            assert!(!darwin_platform_signature(required | untrusted));
        }
        assert!(!darwin_platform_signature(
            CS_VALID | CS_SIGNED | CS_PLATFORM_PATH
        ));
    }

    #[test]
    fn platform_policy_requires_a_valid_seal_on_the_readonly_root_volume() {
        // Real root mounts carry unrelated flags; these must not turn a
        // bitmask requirement into an exact-equality check.
        const MNT_JOURNALED: u32 = 0x0080_0000;
        const MNT_MULTILABEL: u32 = 0x0400_0000;
        let root_mount = 0x4001 | MNT_JOURNALED | MNT_MULTILABEL;
        let seal = 0x0200_0000;
        assert!(darwin_sealed_root_volume(root_mount, seal, seal));
        assert!(
            !darwin_sealed_root_volume(root_mount & !1, seal, seal),
            "writable root"
        );
        assert!(
            !darwin_sealed_root_volume(root_mount & !0x4000, seal, seal),
            "other sealed volume"
        );
        assert!(
            !darwin_sealed_root_volume(root_mount, 0, seal),
            "seal capability not valid"
        );
        assert!(
            !darwin_sealed_root_volume(root_mount, seal, 0),
            "unsealed root"
        );
    }

    #[test]
    fn daemon_paths_allow_system_library_bundles_but_exclude_cli_and_other_apps() {
        for path in [
            "/sbin/launchd",
            "/usr/sbin/daemon",
            "/usr/libexec/daemon",
            "/System/Library/PrivateFrameworks/X/daemon",
            "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder",
            "/System/Library/X.appex/daemon",
        ] {
            assert!(darwin_daemon_path(std::path::Path::new(path)), "{path}");
        }
        for path in [
            "/bin/sleep",
            "/usr/bin/zsh",
            "/Library/Apple/usr/libexec/usbmuxd",
            "/Applications/X.app/daemon",
            "/System/Applications/X.app/daemon",
            "/usr/libexec-evil/daemon",
            "/usr/libexec/../bin/sleep",
            "/sbin",
            "sbin/daemon",
        ] {
            assert!(!darwin_daemon_path(std::path::Path::new(path)), "{path}");
        }
    }

    #[test]
    fn linux_cwd_errors_distinguish_denied_gone_and_query_failures() {
        for (errno, expected) in [
            (libc::EACCES, Some(CwdObservation::PermissionDenied)),
            (libc::EPERM, Some(CwdObservation::PermissionDenied)),
            (libc::EIO, None),
            (libc::EINVAL, None),
            (libc::ESRCH, Some(CwdObservation::Gone)),
            (libc::ENOENT, Some(CwdObservation::Gone)),
        ] {
            let result = map_cwd_error(42, io::Error::from_raw_os_error(errno));
            match expected {
                Some(expected) => assert_eq!(result.unwrap(), expected, "errno {errno}"),
                None => assert!(
                    matches!(result, Err(PidError::Io { pid: 42, source }) if source.raw_os_error() == Some(errno)),
                    "errno {errno} must remain a query error"
                ),
            }
        }
    }

    #[test]
    fn darwin_zero_result_ignores_stale_errno_and_short_records_are_typed() {
        assert!(matches!(
            classify_darwin_result(
                42,
                0,
                24,
                Some(io::Error::from_raw_os_error(libc::EACCES)),
                || DarwinLiveness::Gone
            ),
            Ok(DarwinCwdCall::Gone)
        ));
        assert!(matches!(
            classify_darwin_result(42, 23, 24, None, || DarwinLiveness::Gone),
            Err(PidError::CwdShortRecord {
                pid: 42,
                returned: 23,
                expected: 24
            })
        ));
    }

    #[test]
    fn darwin_zero_result_needs_a_proven_absence_before_reporting_gone() {
        // A live process whose cwd libproc refuses to report is unplaceable.
        for liveness in [DarwinLiveness::Alive, DarwinLiveness::Unknown] {
            let result = classify_darwin_result(42, 0, 24, None, || liveness);
            assert!(
                matches!(result, Err(PidError::Io { .. })),
                "{liveness:?} returned {result:?}"
            );
        }
        // Only a probe that proves ESRCH reports the process gone.
        assert!(matches!(
            classify_darwin_result(42, 0, 24, None, || DarwinLiveness::Gone),
            Ok(DarwinCwdCall::Gone)
        ));
    }

    #[test]
    fn only_actual_eperm_can_reach_the_daemon_exception() {
        for size in [0, -1] {
            assert!(matches!(
                classify_darwin_result(
                    42,
                    size,
                    24,
                    Some(io::Error::from_raw_os_error(libc::EPERM)),
                    || DarwinLiveness::Alive
                ),
                Ok(DarwinCwdCall::PermissionDenied)
            ));
            for errno in [libc::EACCES, libc::EIO, libc::EINVAL] {
                assert!(matches!(
                    classify_darwin_result(
                        42,
                        size,
                        24,
                        Some(io::Error::from_raw_os_error(errno)),
                        || DarwinLiveness::Alive
                    ),
                    Err(PidError::Io { .. })
                ));
            }
        }
        assert!(matches!(
            classify_darwin_result(42, 0, 24, Some(io::Error::from_raw_os_error(0)), || {
                DarwinLiveness::Alive
            }),
            Err(PidError::Io { .. })
        ));
        assert!(matches!(
            classify_darwin_result(
                42,
                -1,
                24,
                Some(io::Error::from_raw_os_error(libc::EPERM)),
                || DarwinLiveness::Alive
            ),
            Ok(DarwinCwdCall::PermissionDenied)
        ));
        assert!(matches!(
            classify_darwin_result(
                42,
                -1,
                24,
                Some(io::Error::from_raw_os_error(libc::ENOTSUP)),
                || DarwinLiveness::Alive
            ),
            Ok(DarwinCwdCall::Unsupported)
        ));
        let malformed = [b'x' as libc::c_char, 0];
        assert!(matches!(
            decode_darwin_path(42, &malformed),
            Err(PidError::MalformedCwdPath { pid: 42, .. })
        ));
    }
}
