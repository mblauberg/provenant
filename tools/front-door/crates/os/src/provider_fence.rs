//! Shared lane leases and exclusive updater custody for one provider.
//!
//! Lock files are stable and never unlinked: the kernel releases their leases
//! when the last holder exits. Provider children inherit shared lane leases;
//! updater groups inherit exclusive leases so runner death cannot drop custody.
use crate::fs::{self, Directory, LockGuard, LockMode};
use provenant_core::{CancelToken, Clock, WaitBudget};
use std::{ffi::OsStr, path::Path};

fn directory(state: &Path, provider: &str) -> fs::Result<Directory> {
    if provider.is_empty()
        || !provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(fs::FsError::InvalidComponent {
            name: provider.into(),
        });
    }
    let root = Directory::open_anchor(state)?;
    match root.mkdir(OsStr::new("provider-updates"), 0o700) {
        Ok(()) => root.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error),
    }
    root.open_verified_directory(OsStr::new("provider-updates"))
}

pub fn acquire(state: &Path, provider: &str, mode: LockMode) -> fs::Result<LockGuard> {
    directory(state, provider)?.try_lock(OsStr::new(&format!("{provider}.lock")), mode)
}

pub fn wait(
    state: &Path,
    provider: &str,
    mode: LockMode,
    budget: WaitBudget,
    cancel: &CancelToken,
    clock: &dyn Clock,
) -> fs::Result<LockGuard> {
    directory(state, provider)?.lock(
        OsStr::new(&format!("{provider}.lock")),
        mode,
        budget,
        cancel,
        clock,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_waits_for_update_then_keeps_its_shared_lease()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let update = acquire(temp.path(), "opencode", LockMode::Exclusive)?;
        let (started, ready) = std::sync::mpsc::channel();
        std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
            let path = temp.path();
            let launch = scope.spawn(move || {
                let clock = crate::clock::SystemClock::new();
                started.send(()).expect("launch observer");
                wait(
                    path,
                    "opencode",
                    LockMode::Shared,
                    WaitBudget::cli(clock.mono_now(), 5).expect("wait budget"),
                    &CancelToken::new(),
                    &clock,
                )
            });
            ready.recv()?;
            assert!(matches!(
                acquire(path, "opencode", LockMode::Shared),
                Err(fs::FsError::LockTimeout { .. })
            ));
            drop(update);
            let lease = launch.join().expect("launch worker")?;
            assert!(matches!(
                acquire(path, "opencode", LockMode::Exclusive),
                Err(fs::FsError::LockTimeout { .. })
            ));
            drop(lease);
            let _update = acquire(path, "opencode", LockMode::Exclusive)?;
            Ok(())
        })
    }

    #[test]
    fn fence_acquire_blocks_launch_and_recovers_after_owner_exit()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let update = acquire(temp.path(), "codex", LockMode::Exclusive)?;
        assert!(matches!(
            acquire(temp.path(), "codex", LockMode::Shared),
            Err(fs::FsError::LockTimeout { .. })
        ));
        // Providers are independent.
        let _other = acquire(temp.path(), "claude", LockMode::Shared)?;
        drop(update);
        let first = acquire(temp.path(), "codex", LockMode::Shared)?;
        let second = acquire(temp.path(), "codex", LockMode::Shared)?;
        assert!(matches!(
            acquire(temp.path(), "codex", LockMode::Exclusive),
            Err(fs::FsError::LockTimeout { .. })
        ));
        drop((first, second));
        let _recovered = acquire(temp.path(), "codex", LockMode::Exclusive)?;
        Ok(())
    }
}
