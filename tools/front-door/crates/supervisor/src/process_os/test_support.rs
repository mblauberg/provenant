//! Test-only native process setup kept inside the OS boundary.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

/// Spawn a process with an inherited attempt mark in a separate session.
pub(crate) fn spawn_setsid_with_attempt_mark(mark: &str) -> std::io::Result<Child> {
    let mut command = Command::new("/bin/sleep");
    command
        .arg("30")
        .env(crate::custody::ATTEMPT_MARK_ENV, mark);
    // SAFETY: the pre-exec hook performs only the async-signal-safe setsid call.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command.spawn()
}
