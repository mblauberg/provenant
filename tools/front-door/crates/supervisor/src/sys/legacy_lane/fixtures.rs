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
pub fn current_start() -> String {
    let row =
        super::platform::process(std::process::id() as i32).expect("fixture process is present");
    super::files::canonical_start(row.epoch)
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
