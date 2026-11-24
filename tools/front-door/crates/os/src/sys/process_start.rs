use provenant_core::WallTime;

/// Kernel creation time of `pid` as wall-clock time, or `None` when missing,
/// invalid or unsupported on this platform.
pub(crate) fn process_start_time(pid: i32) -> Option<WallTime> {
    if pid <= 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        darwin_process_start_time(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_process_start_time(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn darwin_process_start_time(pid: i32) -> Option<WallTime> {
    // Public Darwin SDK: sys/sysctl.h `kinfo_proc` begins with `kp_proc`;
    // sys/proc.h `extern_proc` begins with the `p_starttime` timeval union.
    // Only this prefix is interpreted; remaining bytes stay opaque.
    #[repr(C)]
    struct KinfoPrefix {
        start: libc::timeval,
        opaque: [u8; 4096],
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    let mut size = 0;
    // SAFETY: the four-element MIB is live; null output requests the record size.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size < std::mem::size_of::<libc::timeval>() || size > 4096 {
        return None;
    }
    let expected = size;
    let mut prefix = KinfoPrefix {
        start: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        opaque: [0; 4096],
    };
    // SAFETY: `prefix` is aligned initialized storage larger than `expected`.
    // The public ABI places a `timeval` at offset zero; `sysctl` borrows both
    // pointers for this call only.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            std::ptr::from_mut(&mut prefix).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size < std::mem::size_of::<libc::timeval>() || size > expected {
        return None;
    }
    timeval_to_wall_time(prefix.start.tv_sec, i64::from(prefix.start.tv_usec))
}

#[cfg(any(target_os = "macos", test))]
fn timeval_to_wall_time(tv_sec: i64, tv_usec: i64) -> Option<WallTime> {
    let seconds = u64::try_from(tv_sec).ok()?;
    let micros = u64::try_from(tv_usec).ok()?;
    if micros >= 1_000_000 {
        return None;
    }
    let total = i64::try_from(seconds.checked_mul(1_000_000)?.checked_add(micros)?).ok()?;
    WallTime::from_micros(total).ok()
}

#[cfg(target_os = "linux")]
fn linux_clock_ticks() -> Option<u64> {
    // SAFETY: `sysconf` reads a scalar kernel configuration constant and retains no pointers.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).ok().filter(|ticks| *ticks > 0)
}

#[cfg(target_os = "linux")]
fn linux_process_start_time(pid: i32) -> Option<WallTime> {
    use std::io::Read;
    const STAT_BOUND: u64 = 64 * 1024;
    let ticks_per_second = linux_clock_ticks()?;
    let boot_seconds = crate::sys::clock::boot_unix_seconds().ok()?;
    let file = std::fs::File::open(format!("/proc/{pid}/stat")).ok()?;
    let mut bytes = Vec::new();
    file.take(STAT_BOUND + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > STAT_BOUND {
        return None;
    }
    let start_ticks = parse_linux_stat_start_ticks(pid, &bytes)?;
    linux_ticks_to_wall_time(boot_seconds, start_ticks, ticks_per_second)
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_stat_start_ticks(pid: i32, raw: &[u8]) -> Option<u64> {
    let open = raw.iter().position(|byte| *byte == b'(')?;
    let close = raw.windows(2).rposition(|pair| pair == b") ")?;
    if close <= open {
        return None;
    }
    let declared = std::str::from_utf8(raw.get(..open)?)
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()?;
    if declared != i64::from(pid) {
        return None;
    }
    let tail = raw.get(close.checked_add(2)?..)?;
    let starttime = tail
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .nth(19)?;
    std::str::from_utf8(starttime).ok()?.parse::<u64>().ok()
}

#[cfg(any(target_os = "linux", test))]
fn linux_ticks_to_wall_time(
    boot_seconds: i64,
    start_ticks: u64,
    ticks_per_second: u64,
) -> Option<WallTime> {
    if boot_seconds <= 0 || ticks_per_second == 0 {
        return None;
    }
    let boot_micros = i128::from(boot_seconds).checked_mul(1_000_000)?;
    let offset_micros = i128::from(start_ticks)
        .checked_mul(1_000_000)?
        .checked_div(i128::from(ticks_per_second))?;
    let total = i64::try_from(boot_micros.checked_add(offset_micros)?).ok()?;
    WallTime::from_micros(total).ok()
}

#[cfg(target_os = "macos")]
pub(crate) fn start_time_for_entries(
    pid: i32,
    entries: &[crate::pid::CensusIgnoreEntry],
) -> Option<WallTime> {
    entries
        .iter()
        .any(|entry| entry.started_before.is_some())
        .then(|| process_start_time(pid))
        .flatten()
}

#[cfg(target_os = "macos")]
pub(crate) fn start_time_unchanged(
    pid: i32,
    expected: Option<WallTime>,
    matched: Option<&crate::pid::CensusIgnoreEntry>,
) -> bool {
    !matched.is_some_and(|entry| entry.started_before.is_some())
        || (expected.is_some() && process_start_time(pid) == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darwin_timeval_conversion_rejects_invalid_and_negative_values() {
        let valid = timeval_to_wall_time(1_780_000_000, 123_456).unwrap();
        assert_eq!(valid.as_micros(), 1_780_000_123_456);
        assert!(timeval_to_wall_time(-1, 0).is_none());
        assert!(timeval_to_wall_time(10, -1).is_none());
        assert!(timeval_to_wall_time(10, 1_000_000).is_none());
        assert!(timeval_to_wall_time(i64::MAX, 0).is_none());
    }

    #[test]
    fn linux_stat_starttime_handles_comm_with_spaces_and_parens() {
        let raw = b"42 (worker ) thread) S 1 42 42 0 -1 4194304 100 0 0 0 12 34 0 0 20 0 1 0 12345 4096 10 0\n";
        assert_eq!(parse_linux_stat_start_ticks(42, raw), Some(12345));
        assert_eq!(parse_linux_stat_start_ticks(99, raw), None);
        assert_eq!(parse_linux_stat_start_ticks(42, b"42 broken"), None);
        let wall = linux_ticks_to_wall_time(1_780_000_000, 250, 100).unwrap();
        assert_eq!(wall.as_micros(), 1_780_000_002_500_000);
        assert!(linux_ticks_to_wall_time(0, 250, 100).is_none());
        assert!(linux_ticks_to_wall_time(1_780_000_000, 250, 0).is_none());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn reads_current_process_start_time_and_rejects_invalid_pids() {
        let pid = i32::try_from(std::process::id()).unwrap();
        assert!(process_start_time(pid).is_some());
        for bad in [0, -1, i32::MIN] {
            assert_eq!(process_start_time(bad), None);
        }
    }
}
