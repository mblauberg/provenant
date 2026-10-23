//! Read-only process and time observations. No signalling authority.
use crate::identity::Presence;
use std::ffi::CString;
use std::io;

pub fn presence(pid: i32) -> Presence {
    // SAFETY: signal zero never sends a signal. No memory
    // is borrowed and errno is thread-local. Only ESRCH proves absence.
    let status = unsafe { libc::kill(pid, 0) };
    if status == 0 {
        Presence::Alive
    } else if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Presence::Gone
    } else {
        Presence::Unknown
    }
}

pub fn clock_ticks() -> io::Result<i64> {
    sysconf(libc::_SC_CLK_TCK)
}
pub fn page_size() -> io::Result<i64> {
    sysconf(libc::_SC_PAGESIZE)
}
fn sysconf(name: i32) -> io::Result<i64> {
    // SAFETY: these are defined query constants, with no pointer arguments,
    // ownership or shared Rust state. Nonpositive results are refused.
    let value = unsafe { libc::sysconf(name) };
    if value > 0 {
        Ok(value as i64)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Canonical English names, local timezone, independent of LC_TIME.
pub fn lstart(epoch: f64) -> io::Result<String> {
    let time = broken_down(epoch)?;
    let day = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        .get(time.tm_wday as usize)
        .ok_or_else(invalid_time)?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .get(time.tm_mon as usize)
    .ok_or_else(invalid_time)?;
    Ok(format!(
        "{day} {month} {:2} {:02}:{:02}:{:02} {}",
        time.tm_mday,
        time.tm_hour,
        time.tm_min,
        time.tm_sec,
        i64::from(time.tm_year) + 1900
    ))
}

fn invalid_time() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid process start time")
}
fn broken_down(epoch: f64) -> io::Result<libc::tm> {
    if !epoch.is_finite() || epoch < i64::MIN as f64 || epoch >= i64::MAX as f64 {
        return Err(invalid_time());
    }
    let epoch = epoch.floor() as libc::time_t;
    // SAFETY: localtime_r writes a live, correctly sized tm and retains no
    // pointers. Its reentrant interface permits concurrent independent calls.
    // The epoch pointer remains live; null is an explicit failure.
    unsafe {
        let mut time = std::mem::MaybeUninit::uninit();
        if libc::localtime_r(&epoch, time.as_mut_ptr()).is_null() {
            return Err(invalid_time());
        }
        Ok(time.assume_init())
    }
}

/// Uses an owned POSIX locale rather than mutating the process-global locale.
/// Empty locale names select LC_ALL/LC_TIME/LANG, as the Python ps edge does.
pub fn locale_start(epoch: f64, locale: &str, darwin: bool) -> io::Result<String> {
    let time = broken_down(epoch)?;
    let locale = CString::new(locale).map_err(|_| invalid_time())?;
    // SAFETY: newlocale returns an owned locale object, never a borrowed global
    // locale. Inputs and tm stay live; strftime_l writes only within buffer.
    // freelocale executes after the last use on all paths. No locale escapes
    // or is shared, and no process environment or locale is mutated.
    unsafe {
        let mut owned = libc::newlocale(libc::LC_ALL_MASK, locale.as_ptr(), std::ptr::null_mut());
        if owned.is_null() {
            owned = libc::newlocale(libc::LC_ALL_MASK, c"C".as_ptr(), std::ptr::null_mut());
        }
        if owned.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = [0_u8; 4096];
        let length = libc::strftime_l(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            c"%c".as_ptr(),
            &time,
            owned,
        );
        libc::freelocale(owned);
        if length == 0 {
            return Err(invalid_time());
        }
        let mut text = String::from_utf8_lossy(&buffer[..length]).into_owned();
        if darwin && length < 28 {
            text.extend(std::iter::repeat_n(' ', 28 - length));
        }
        Ok(text)
    }
}

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "macos")]
pub use darwin::*;

/// Node/libuv's standard signal names, without locale-dependent strsignal text.
pub fn signal_name(signal: u32) -> Option<&'static str> {
    let names = [
        (libc::SIGHUP, "SIGHUP"),
        (libc::SIGINT, "SIGINT"),
        (libc::SIGQUIT, "SIGQUIT"),
        (libc::SIGILL, "SIGILL"),
        (libc::SIGTRAP, "SIGTRAP"),
        (libc::SIGABRT, "SIGABRT"),
        (libc::SIGBUS, "SIGBUS"),
        (libc::SIGFPE, "SIGFPE"),
        (libc::SIGKILL, "SIGKILL"),
        (libc::SIGUSR1, "SIGUSR1"),
        (libc::SIGSEGV, "SIGSEGV"),
        (libc::SIGUSR2, "SIGUSR2"),
        (libc::SIGPIPE, "SIGPIPE"),
        (libc::SIGALRM, "SIGALRM"),
        (libc::SIGTERM, "SIGTERM"),
        (libc::SIGCHLD, "SIGCHLD"),
        (libc::SIGCONT, "SIGCONT"),
        (libc::SIGSTOP, "SIGSTOP"),
        (libc::SIGTSTP, "SIGTSTP"),
        (libc::SIGTTIN, "SIGTTIN"),
        (libc::SIGTTOU, "SIGTTOU"),
        (libc::SIGURG, "SIGURG"),
        (libc::SIGXCPU, "SIGXCPU"),
        (libc::SIGXFSZ, "SIGXFSZ"),
        (libc::SIGVTALRM, "SIGVTALRM"),
        (libc::SIGPROF, "SIGPROF"),
        (libc::SIGWINCH, "SIGWINCH"),
        (libc::SIGIO, "SIGIO"),
        (libc::SIGSYS, "SIGSYS"),
    ];
    names
        .into_iter()
        .chain(platform_signals())
        .find_map(|(value, name)| (value as u32 == signal).then_some(name))
}

#[cfg(target_os = "macos")]
fn platform_signals() -> impl IntoIterator<Item = (i32, &'static str)> {
    [(libc::SIGINFO, "SIGINFO")]
}
#[cfg(target_os = "linux")]
fn platform_signals() -> impl IntoIterator<Item = (i32, &'static str)> {
    [(libc::SIGSTKFLT, "SIGSTKFLT"), (libc::SIGPWR, "SIGPWR")]
}
