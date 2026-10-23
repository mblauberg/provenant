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
#[cfg(feature = "fixture")]
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
    let epoch = epoch as libc::time_t;
    // SAFETY: zero is a valid initial representation for tm; localtime_r
    // overwrites the record before its fields are read.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: epoch and tm are live for the call; localtime_r writes only to
    // tm and retains neither pointer.
    assert!(!unsafe { libc::localtime_r(&epoch, &mut tm) }.is_null());
    let mut buf = [0u8; 80];
    // SAFETY: buf is live writable storage of the declared size, the format is
    // static NUL-terminated data, and tm is initialized by localtime_r.
    let n = unsafe { libc::strftime(buf.as_mut_ptr().cast(), 80, c"%a %b %e %T %Y".as_ptr(), &tm) };
    assert!(n > 0);
    String::from_utf8_lossy(&buf[..n]).into_owned()
}
pub fn inherit_ignored_signals(command: &mut Command) {
    let prepare = || {
        // SAFETY: signal takes scalar values and changes only this fixture
        // child before exec.
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
        // SAFETY: signal takes scalar values and changes only this fixture
        // child before exec.
        unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
        // SAFETY: zero is a valid initial representation for sigset_t;
        // sigemptyset initialises it before use.
        let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: mask is live writable storage for sigemptyset.
        unsafe { libc::sigemptyset(&mut mask) };
        // SAFETY: mask is initialized and SIGTERM is a valid scalar signal.
        unsafe { libc::sigaddset(&mut mask, libc::SIGTERM) };
        // SAFETY: mask is initialized and no output storage is requested.
        unsafe { libc::sigprocmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) };
        Ok(())
    };
    // SAFETY: the hook is installed before spawn and its callback uses only
    // async-signal-safe libc calls with stack storage; it takes no Rust locks.
    unsafe { command.pre_exec(prepare) };
}
