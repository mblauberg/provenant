//! Boot time and host monotonic sampling: reject process records from an
//! earlier boot, and measure intervals every process on the host agrees on.
use std::io;

#[cfg(target_os = "macos")]
fn kern_boottime() -> io::Result<libc::timeval> {
    let mut boot = std::mem::MaybeUninit::<libc::timeval>::uninit();
    let expected = std::mem::size_of::<libc::timeval>();
    let mut length = expected;
    // SAFETY: kern.boottime is NUL-terminated and read-only; boot is aligned
    // writable storage of length bytes. sysctl retains no pointers.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            boot.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    if length != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short boot time record",
        ));
    }
    // SAFETY: successful sysctl returned exactly the complete timeval record.
    let boot = unsafe { boot.assume_init() };
    if boot.tv_sec <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid boot time",
        ));
    }
    Ok(boot)
}

#[cfg(target_os = "macos")]
pub(crate) fn boot_unix_seconds() -> io::Result<i64> {
    Ok(kern_boottime()?.tv_sec)
}

#[cfg(target_os = "linux")]
pub(crate) fn boot_unix_seconds() -> io::Result<i64> {
    parse_linux_boot(&std::fs::read_to_string("/proc/stat")?)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn boot_unix_seconds() -> io::Result<i64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "boot time unavailable",
    ))
}

/// Names the current boot: `kern.boottime` to the microsecond.
#[cfg(target_os = "macos")]
pub(crate) fn boot_identity() -> io::Result<String> {
    let boot = kern_boottime()?;
    Ok(format!("{}.{:06}", boot.tv_sec, boot.tv_usec))
}

/// Names the current boot: the kernel's random boot id.
#[cfg(target_os = "linux")]
pub(crate) fn boot_identity() -> io::Result<String> {
    parse_boot_id(&std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn boot_identity() -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "boot identity unavailable",
    ))
}

/// Microseconds on the host's monotonic clock since boot, shared by every
/// process on the host. macOS `CLOCK_MONOTONIC` and Linux `CLOCK_BOOTTIME`
/// both keep counting while the host sleeps.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn host_monotonic_micros() -> io::Result<u64> {
    #[cfg(target_os = "macos")]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    #[cfg(target_os = "linux")]
    const CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
    let mut now = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: now is aligned writable storage for one timespec and
    // clock_gettime retains no pointer.
    let status = unsafe { libc::clock_gettime(CLOCK, now.as_mut_ptr()) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful clock_gettime fully initialised the timespec.
    let now = unsafe { now.assume_init() };
    // The field types differ between targets (time_t, c_long), so widen.
    #[allow(clippy::useless_conversion)]
    let (seconds, nanos) = (i64::from(now.tv_sec), i64::from(now.tv_nsec));
    micros_since_boot(seconds, nanos)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn host_monotonic_micros() -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "host monotonic clock unavailable",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn micros_since_boot(seconds: i64, nanos: i64) -> io::Result<u64> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid monotonic time");
    if seconds < 0 || !(0..1_000_000_000).contains(&nanos) {
        return Err(invalid());
    }
    u64::try_from(seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000))
        .and_then(|micros| micros.checked_add(u64::try_from(nanos / 1000).ok()?))
        .ok_or_else(invalid)
}

#[cfg(any(target_os = "linux", test))]
fn parse_boot_id(text: &str) -> io::Result<String> {
    let id = text.trim();
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid or missing boot id",
        ));
    }
    Ok(id.to_owned())
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_boot(text: &str) -> io::Result<i64> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid or missing boot time");
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("btime") {
            continue;
        }
        let seconds = fields.next().and_then(|value| value.parse::<i64>().ok());
        return match seconds {
            Some(seconds) if seconds > 0 && fields.next().is_none() => Ok(seconds),
            _ => Err(invalid()),
        };
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_boot_time_is_numeric_and_ignores_unrelated_stat_records() {
        assert_eq!(
            parse_linux_boot("cpu 12 34\nbtime 1780000000\nprocesses 42\n").unwrap(),
            1_780_000_000
        );
        for text in [
            "",
            "btime invalid",
            "btime -1",
            "btime 0",
            "btime 42 extra",
            "btime 9223372036854775808",
        ] {
            assert_eq!(
                parse_linux_boot(text).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn kernel_boot_time_is_a_positive_unix_timestamp() {
        assert!(boot_unix_seconds().unwrap() > 0);
    }

    #[test]
    fn boot_id_is_one_trimmed_hex_token() {
        let id = "0f8e2f4a-71c4-4d52-9a40-3b1d5a6c7e90";
        assert_eq!(parse_boot_id(&format!("{id}\n")).unwrap(), id);
        for text in ["", "\n", "not a boot id", "0f8e\n2f4a", &"a".repeat(65)] {
            assert_eq!(
                parse_boot_id(text).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn monotonic_time_converts_to_whole_microseconds_or_fails() {
        assert_eq!(micros_since_boot(3, 123_456_789).unwrap(), 3_123_456);
        for (seconds, nanos) in [(-1, 0), (0, -1), (0, 1_000_000_000), (i64::MAX, 0)] {
            assert_eq!(
                micros_since_boot(seconds, nanos).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn host_monotonic_clock_does_not_go_backwards_and_the_boot_identity_is_stable() {
        let first = host_monotonic_micros().unwrap();
        assert!(first > 0);
        assert!(host_monotonic_micros().unwrap() >= first);
        assert_eq!(boot_identity().unwrap(), boot_identity().unwrap());
    }
}
