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
    Ok(blocks.saturating_mul(size))
}

#[cfg(test)]
mod tests {
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
