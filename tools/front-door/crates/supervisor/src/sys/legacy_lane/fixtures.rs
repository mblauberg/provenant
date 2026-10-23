//! Syscall fixtures for the binary's existing lane integration tests.
use std::ffi::CStr;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

pub const NONBLOCK: i32 = libc::O_NONBLOCK;
pub const KILL: i32 = libc::SIGKILL;
pub const TERM: i32 = libc::SIGTERM;

pub fn new_session() -> i32 {
    // SAFETY: setsid takes no pointers and changes only the calling process.
    unsafe { libc::setsid() }
}
pub fn signal(pid: i32, signal: i32) -> i32 {
    // SAFETY: kill takes scalar identifiers; the kernel validates them.
    unsafe { libc::kill(pid, signal) }
}
pub fn fifo(name: &CStr) -> i32 {
    // SAFETY: name is a live NUL-terminated path throughout the call.
    unsafe { libc::mkfifo(name.as_ptr(), 0o600) }
}
pub fn wait_tick() {
    // SAFETY: a null poll array with zero entries is a bounded kernel wait.
    unsafe { libc::poll(std::ptr::null_mut(), 0, 10) };
}
pub fn make_inheritable(file: &File) {
    // SAFETY: file owns the descriptor throughout the scalar fcntl call.
    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) };
}
pub fn lock(file: &File) -> i32 {
    // SAFETY: file owns the descriptor; flock validates the scalar operation.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }
}
pub fn process_group() -> i32 {
    // SAFETY: getpgrp takes no arguments and returns a process identifier.
    unsafe { libc::getpgrp() }
}
/// Independent oracle: do not call production process or timestamp helpers.
pub fn independent_start() -> String {
    #[cfg(target_os = "macos")]
    // SAFETY: the fixed libproc buffer has the requested ABI size, and the
    // current PID remains alive throughout this synchronous query.
    let epoch = unsafe {
        #[link(name = "proc")]
        unsafe extern "C" {
            fn proc_pidinfo(
                pid: i32,
                flavor: i32,
                arg: u64,
                buffer: *mut libc::c_void,
                size: i32,
            ) -> i32;
        }
        let mut bsd = [0u64; 17];
        assert_eq!(
            proc_pidinfo(
                std::process::id() as i32,
                3,
                0,
                bsd.as_mut_ptr().cast(),
                136
            ),
            136
        );
        bsd[15]
    };
    #[cfg(target_os = "linux")]
    let epoch = {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let start = stat[stat.rfind(") ").unwrap() + 2..]
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let boot = std::fs::read_to_string("/proc/stat")
            .unwrap()
            .lines()
            .find_map(|s| s.strip_prefix("btime ").and_then(|s| s.parse::<u64>().ok()))
            .unwrap();
        // SAFETY: sysconf accepts this scalar key and returns clock ticks.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        assert!(ticks > 0);
        boot + start / ticks as u64
    };
    // SAFETY: localtime_r writes only to our live tm; strftime writes at most
    // the provided buffer length and receives a NUL-terminated format.
    unsafe {
        let epoch = epoch as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        assert!(!libc::localtime_r(&epoch, &mut tm).is_null());
        let mut buf = [0u8; 80];
        let n = libc::strftime(buf.as_mut_ptr().cast(), 80, c"%a %b %e %T %Y".as_ptr(), &tm);
        assert!(n > 0);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}
pub fn inherit_ignored_signals(command: &mut Command) {
    // SAFETY: the closure calls only async-signal-safe libc functions after
    // fork, uses stack storage, and neither allocates nor takes Rust locks.
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGCHLD, libc::SIG_IGN);
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGTERM);
            libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
            Ok(())
        });
    }
}
