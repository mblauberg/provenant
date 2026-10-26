//! Boot time sampling used to reject process records from an earlier boot.
use std::io;

#[cfg(target_os = "macos")]
pub(crate) fn boot_unix_seconds() -> io::Result<i64> {
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
    Ok(boot.tv_sec)
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
}
