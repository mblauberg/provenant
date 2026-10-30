//! Admission resource samples. A failed sample is an error for the caller to
//! queue on, never an inferred amount of free memory or disk.
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Bytes the kernel can hand to a new process without paging out live work,
/// and installed physical memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySample {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

/// Kernel memory pressure, where available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryPressure {
    Normal,
    Warning,
    Critical,
}

#[cfg(target_os = "linux")]
pub fn memory_pressure() -> io::Result<Option<MemoryPressure>> {
    Ok(None)
}

#[cfg(target_os = "macos")]
pub fn memory_pressure() -> io::Result<Option<MemoryPressure>> {
    let mut level: libc::c_int = 0;
    let mut length = std::mem::size_of_val(&level);
    // SAFETY: the name is NUL terminated and level is a writable integer;
    // sysctl retains no pointers.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&raw mut level).cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length != std::mem::size_of_val(&level) {
        return Err(io::Error::other("invalid memory pressure size"));
    }
    match level {
        1 => Ok(Some(MemoryPressure::Normal)),
        2 => Ok(Some(MemoryPressure::Warning)),
        4 => Ok(Some(MemoryPressure::Critical)),
        _ => Err(io::Error::other("unknown memory pressure level")),
    }
}

/// Linux reads `MemAvailable`; it already accounts for reclaimable caches.
#[cfg(target_os = "linux")]
pub fn memory() -> io::Result<MemorySample> {
    let text = std::fs::read_to_string("/proc/meminfo")?;
    let field = |name: &str| -> io::Result<u64> {
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "meminfo field missing"))?;
        let kib = line
            .trim()
            .strip_suffix("kB")
            .and_then(|value| value.trim().parse::<u64>().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "meminfo field invalid"))?;
        kib.checked_mul(1024)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "meminfo overflow"))
    };
    Ok(MemorySample {
        available_bytes: field("MemAvailable:")?,
        total_bytes: field("MemTotal:")?,
    })
}

/// macOS counts free, inactive and speculative pages, matching `vm_stat`.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
pub fn memory() -> io::Result<MemorySample> {
    let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: mach_host_self takes no arguments and returns a send right for
    // the host port; it touches no Rust memory.
    let host = unsafe { libc::mach_host_self() };
    // SAFETY: stats is a live, writable vm_statistics64 buffer whose capacity
    // in natural-sized words is passed in count; the kernel retains no pointer.
    let status = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast(),
            &mut count,
        )
    };
    // Kernels fill fewer words than the libc record on some releases; the
    // counters used here sit at its start, and the rest stays zeroed.
    let needed = (std::mem::offset_of!(libc::vm_statistics64, speculative_count)
        + std::mem::size_of::<libc::natural_t>())
    .div_ceil(std::mem::size_of::<libc::integer_t>());
    if status != libc::KERN_SUCCESS || (count as usize) < needed {
        return Err(io::Error::other(format!(
            "host_statistics64 failed: status {status}, {count} words"
        )));
    }
    // SAFETY: the buffer was zero-initialised and the kernel wrote at least
    // the counters read below; every field is a plain integer.
    let stats = unsafe { stats.assume_init() };
    // SAFETY: sysconf reads a constant configuration value.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = u64::try_from(page).map_err(|_| io::Error::other("page size unavailable"))?;
    let pages = u64::from(stats.free_count)
        .saturating_add(u64::from(stats.inactive_count))
        .saturating_add(u64::from(stats.speculative_count));
    let mut total = 0_u64;
    let mut length = std::mem::size_of::<u64>();
    // SAFETY: the name is NUL-terminated; total is a live u64 whose exact size
    // is passed in length. sysctl retains no pointer.
    let result = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut total).cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || length != std::mem::size_of::<u64>() || total == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(MemorySample {
        available_bytes: pages.saturating_mul(page),
        total_bytes: total,
    })
}

/// The user's process table: how many processes the uid holds against the
/// `RLIMIT_NPROC` soft limit. A `limit` of `u64::MAX` means unlimited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessBudget {
    pub used: u64,
    pub limit: u64,
}

impl ProcessBudget {
    pub const UNLIMITED: Self = Self {
        used: 0,
        limit: u64::MAX,
    };

    /// Processes kept free for the user's own tools and for the recovery that
    /// ends a runaway: a tenth of the limit, and never fewer than 64.
    pub fn headroom(&self) -> u64 {
        (self.limit / 10).max(64)
    }

    /// Whether a new lane or worker may start while leaving the headroom.
    pub fn has_room(&self) -> bool {
        self.used.saturating_add(self.headroom()) < self.limit
    }
}

/// Sample this user's process count and limit. One probe costs about one
/// sysctl on macOS and a `/proc` walk on Linux, so callers probe at admission
/// and detached spawns, never per command.
pub fn process_budget() -> io::Result<ProcessBudget> {
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::zeroed();
    // SAFETY: limit is a live, writable rlimit and the kernel retains no pointer.
    let result = unsafe { libc::getrlimit(libc::RLIMIT_NPROC, limit.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful getrlimit initialised the record.
    let limit = unsafe { limit.assume_init() }.rlim_cur;
    let limit = if limit == libc::RLIM_INFINITY {
        u64::MAX
    } else {
        #[allow(clippy::useless_conversion)]
        u64::from(limit)
    };
    Ok(ProcessBudget {
        used: user_process_count()?,
        limit,
    })
}

#[cfg(target_os = "macos")]
fn user_process_count() -> io::Result<u64> {
    /// `PROC_UID_ONLY` from `<libproc.h>`; the libc crate does not export it.
    const PROC_UID_ONLY: u32 = 4;
    // SAFETY: getuid has no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    // A null buffer returns a size for every process on the host, not just
    // this uid's, so fill a buffer and count the entries the kernel writes.
    // SAFETY: a null buffer with size 0 asks only for a byte count; no memory
    // is written.
    let estimate = unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
    let estimate = usize::try_from(estimate).map_err(|_| io::Error::last_os_error())? / PID_BYTES;
    let mut slots = estimate.saturating_add(64);
    for _ in 0..4 {
        let mut pids = vec![0 as libc::pid_t; slots];
        let capacity = i32::try_from(
            pids.len()
                .checked_mul(PID_BYTES)
                .ok_or_else(|| io::Error::other("invalid process list size"))?,
        )
        .map_err(|_| io::Error::other("invalid process list size"))?;
        // SAFETY: pids is writable for capacity bytes and the kernel retains no pointer.
        let bytes =
            unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, pids.as_mut_ptr().cast(), capacity) };
        let bytes = usize::try_from(bytes).map_err(|_| io::Error::last_os_error())?;
        if bytes < capacity as usize {
            return Ok(pids[..bytes / PID_BYTES]
                .iter()
                .filter(|pid| **pid != 0)
                .count() as u64);
        }
        slots = slots
            .checked_mul(2)
            .ok_or_else(|| io::Error::other("invalid process list size"))?;
    }
    Err(io::Error::other("process list remained full after retries"))
}

#[cfg(target_os = "macos")]
const PID_BYTES: usize = std::mem::size_of::<libc::pid_t>();

/// Linux counts tasks, because `RLIMIT_NPROC` does: every thread of a process
/// the user owns uses an entry.
#[cfg(target_os = "linux")]
fn user_process_count() -> io::Result<u64> {
    // SAFETY: getuid has no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    count_linux_tasks(std::path::Path::new("/proc"), uid)
}

#[cfg(any(target_os = "linux", test))]
fn count_linux_tasks(proc: &std::path::Path, uid: libc::uid_t) -> io::Result<u64> {
    let mut tasks = 0_u64;
    for entry in std::fs::read_dir(proc)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()))
        {
            continue;
        }
        let status = match std::fs::read_to_string(entry.path().join("status")) {
            Ok(status) => status,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let real_uid = status
            .lines()
            .find_map(|line| {
                line.strip_prefix("Uid:")
                    .and_then(|value| value.split_whitespace().next())
            })
            .and_then(|value| value.parse::<libc::uid_t>().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "missing real uid in process status",
                )
            })?;
        if real_uid != uid {
            continue;
        }
        let threads = match std::fs::read_dir(entry.path().join("task")) {
            Ok(threads) => threads,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for thread in threads {
            match thread {
                Ok(_) => tasks = tasks.saturating_add(1),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
        }
    }
    Ok(tasks)
}

/// Bytes available to this unprivileged user on the filesystem holding `file`.
pub(crate) fn available_bytes(file: &File) -> io::Result<u64> {
    let stats = filesystem(file)?;
    #[allow(clippy::useless_conversion)]
    let blocks = u64::from(stats.f_bavail);
    #[allow(clippy::useless_conversion)]
    let size = u64::from(stats.f_frsize);
    Ok(blocks.saturating_mul(size))
}

pub(crate) fn total_bytes(file: &File) -> io::Result<u64> {
    let stats = filesystem(file)?;
    #[allow(clippy::useless_conversion)]
    let blocks = u64::from(stats.f_blocks);
    #[allow(clippy::useless_conversion)]
    let size = u64::from(stats.f_frsize);
    Ok(blocks.saturating_mul(size))
}

fn filesystem(file: &File) -> io::Result<libc::statvfs> {
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: the descriptor is owned by file for the call; stats is a live,
    // writable statvfs buffer and the kernel retains no pointer.
    let result = unsafe { libc::fstatvfs(file.as_raw_fd(), stats.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful fstatvfs initialised the record.
    Ok(unsafe { stats.assume_init() })
}

/// Sample an existing path's filesystem with one syscall, without opening it.
pub(crate) fn available_bytes_at(path: &Path) -> io::Result<u64> {
    filesystem_bytes_at(path).map(|(available, _)| available)
}

pub(crate) fn filesystem_bytes_at(path: &Path) -> io::Result<(u64, u64)> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    #[cfg(target_os = "macos")]
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    #[cfg(target_os = "linux")]
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: path is NUL-terminated and stats is a live, writable record of
    // the exact type required by the call; the kernel retains neither pointer.
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) };
    // SAFETY: same pointer and buffer guarantees as the macOS call above.
    #[cfg(target_os = "linux")]
    let result = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful syscall initialised the record.
    let stats = unsafe { stats.assume_init() };
    #[allow(clippy::useless_conversion)]
    let blocks = u64::from(stats.f_bavail);
    #[cfg(target_os = "macos")]
    let size = u64::from(stats.f_bsize);
    #[cfg(target_os = "linux")]
    let size = stats.f_frsize;
    #[allow(clippy::useless_conversion)]
    let total = u64::from(stats.f_blocks).saturating_mul(size);
    Ok((blocks.saturating_mul(size), total))
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts_tasks_by_real_uid_and_rejects_invalid_status() -> std::io::Result<()> {
        let proc = tempfile::tempdir()?;
        for (pid, status, count) in [
            ("101", "Uid:\t42\t0\t0\t0\n", 3),
            ("102", "Uid:\t43\t42\t42\t42\n", 2),
        ] {
            let process = proc.path().join(pid);
            std::fs::create_dir_all(process.join("task"))?;
            std::fs::write(process.join("status"), status)?;
            for task in 0..count {
                std::fs::create_dir(process.join("task").join(task.to_string()))?;
            }
        }
        // A vanished process is ignored, including one that lost its task directory.
        std::fs::create_dir(proc.path().join("103"))?;
        std::fs::create_dir(proc.path().join("104"))?;
        std::fs::write(proc.path().join("104/status"), "Uid:\t42\t42\t42\t42\n")?;
        assert_eq!(super::count_linux_tasks(proc.path(), 42)?, 3);
        std::fs::write(proc.path().join("104/status"), "Name:\tbroken\n")?;
        assert_eq!(
            super::count_linux_tasks(proc.path(), 42)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        Ok(())
    }
    #[test]
    fn samples_are_plausible() {
        let memory = super::memory().unwrap();
        assert!(memory.total_bytes > 0 && memory.available_bytes <= memory.total_bytes);
        let file = std::fs::File::open("/").unwrap();
        let available = super::available_bytes(&file).unwrap();
        let total = super::total_bytes(&file).unwrap();
        assert!(available > 0 && available <= total);
    }

    #[test]
    fn process_budget_counts_this_users_processes() {
        let budget = super::process_budget().unwrap();
        assert!(budget.used > 0, "this test process is in the count");
        assert!(budget.limit >= budget.used);
        // Only this uid's processes count, never the whole host's.
        #[cfg(target_os = "macos")]
        {
            // `PROC_ALL_PIDS` is 1. SAFETY: a null buffer with size 0 only asks
            // for a byte count.
            let host = unsafe { libc::proc_listpids(1, 0, std::ptr::null_mut(), 0) };
            assert!(budget.used < u64::try_from(host).unwrap() / 4);
        }
        assert!(super::ProcessBudget::UNLIMITED.has_room());
    }

    #[test]
    fn headroom_is_a_tenth_of_the_limit_and_at_least_sixty_four() {
        let at = |used, limit| super::ProcessBudget { used, limit }.has_room();
        assert!(at(0, 2666));
        assert!(at(2399, 2666), "headroom 266");
        assert!(!at(2400, 2666));
        assert!(at(99, 200), "headroom is 64 for a small limit");
        assert!(!at(136, 200));
        assert!(!at(10, 10));
    }

    #[test]
    fn path_sample_uses_unprivileged_available_space() {
        assert!(super::available_bytes_at(std::path::Path::new("/")).unwrap() > 0);
        assert_eq!(
            super::available_bytes_at(std::path::Path::new("/\0invalid"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EINVAL)
        );
    }
}
