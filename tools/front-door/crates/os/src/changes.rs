//! Owner-issued change signal, stored in one locked, private state file.
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use crate::fs::{Directory, FsError, LockMode, LockRefusal};
use crate::sys::fs as os;
pub use provenant_core::changes::{FILE_NAME, Generations, Scope};

/// Bounded lock attempts before a bump is skipped. The signal is a hint, so a
/// busy signal costs at most about 50 ms and the next commit bumps again.
const LOCK_ATTEMPTS: u32 = 5;
const LOCK_RETRY: Duration = Duration::from_millis(10);

/// A bounded, non-blocking read. A busy, missing or stray signal is unknown.
///
/// Returns `Generations::default()` (all fields `None`) when the signal file
/// is absent, held exclusively by a writer, or refused as a stray entry;
/// callers must treat `None` generation values as unknown and do a full
/// bounded re-read. The next owner bump replaces a stray entry.
pub fn read(state_dir: &Path) -> io::Result<Generations> {
    let dir = match Directory::open_anchor(state_dir) {
        Ok(dir) => dir,
        Err(e) if e.is_not_found() => return Ok(Generations::default()),
        Err(e) => return Err(e.into()),
    };
    let name = OsStr::new(FILE_NAME);
    let file = match dir.signal_file(name, false) {
        Ok(file) => file,
        Err(e) if e.is_not_found() || is_stray(&e) => return Ok(Generations::default()),
        Err(e) => return Err(e.into()),
    };
    match os::try_flock(&file, LockMode::Shared) {
        Ok(()) => (),
        // A writer holds the lock; return unknown rather than blocking.
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Generations::default()),
        Err(e) => return Err(e),
    }
    dir.verify_signal_file(name, &file)
        .map_err(io::Error::from)?;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    Ok(if bytes.len() > 4096 {
        Generations::default()
    } else {
        serde_json::from_slice(&bytes).unwrap_or_default()
    })
}

/// Bump one scope after an owner commit. See `bump_scopes` for multiple scopes.
pub fn bump(state_dir: &Path, scope: Scope) -> io::Result<()> {
    bump_scopes(state_dir, &[scope])
}

/// Post-commit hint for an owner write that has already committed. A failed
/// bump never fails that write. The first failure per process is reported on
/// stderr; later failures are silent so a broken signal does not spam.
#[allow(clippy::print_stderr)]
pub fn hint(state_dir: &Path, scopes: &[Scope]) {
    static REPORTED: std::sync::Once = std::sync::Once::new();
    if let Err(error) = bump_scopes(state_dir, scopes) {
        REPORTED.call_once(|| {
            eprintln!(
                "warning: change signal not published after a committed write; readers keep their last generation until a later write or a full re-read ({error})"
            );
        });
    }
}

/// Bump several scopes at once, taking the signal lock exactly once.
///
/// The lock is taken with a short bounded retry. On timeout the bump is
/// skipped and `Ok` is returned: the signal is a hint, not commit durability.
/// The write is not fsynced. A power loss can roll the signal back to an
/// earlier generation; the owner's commit itself is already durable.
pub fn bump_scopes(state_dir: &Path, scopes: &[Scope]) -> io::Result<()> {
    if scopes.is_empty() {
        return Ok(());
    }
    let dir = Directory::open_anchor(state_dir).map_err(io::Error::from)?;
    let name = OsStr::new(FILE_NAME);
    let mut file = match dir.signal_file(name, true) {
        Ok(file) => file,
        Err(error) if is_stray(&error) => {
            // Replace the entry without writing through it. The new file has
            // a new epoch, which tells readers the signal was reset.
            dir.unlink(name).map_err(io::Error::from)?;
            dir.signal_file(name, true).map_err(io::Error::from)?
        }
        Err(error) => return Err(io::Error::from(error)),
    };
    if !lock_bounded(&file)? {
        // Another writer holds the signal and will bump on its own commit.
        return Ok(());
    }

    dir.verify_signal_file(name, &file)
        .map_err(io::Error::from)?;
    let mut bytes = Vec::new();
    (&mut file).take(4097).read_to_end(&mut bytes)?;
    let mut generations: Generations = if bytes.len() > 4096 {
        Generations::default()
    } else {
        serde_json::from_slice(&bytes).unwrap_or_default()
    };
    if generations.epoch.is_none() {
        let entropy = crate::fs::entropy_128().map_err(io::Error::from)?;
        generations.epoch = Some(
            u64::from_le_bytes(entropy[..8].try_into().map_err(io::Error::other)?)
                & ((1_u64 << 53) - 1),
        );
    }
    for scope in scopes {
        let value = match scope {
            Scope::Inbox => &mut generations.inbox,
            Scope::Board => &mut generations.board,
            Scope::Chair => &mut generations.chair,
            Scope::Quota => &mut generations.quota,
            Scope::Configuration => &mut generations.configuration,
        };
        *value = Some(
            value
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| io::Error::other("change generation exhausted"))?,
        );
    }
    let data = serde_json::to_vec(&generations).map_err(io::Error::other)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&data)?;
    file.set_len(data.len() as u64)?;
    // No fsync: this is a hint, and the commit it follows is already durable.
    Ok(())
}

/// Take the exclusive signal lock within the bounded retry. `false` on timeout.
fn lock_bounded(file: &File) -> io::Result<bool> {
    for attempt in 0..LOCK_ATTEMPTS {
        match os::try_flock(file, LockMode::Exclusive) {
            Ok(()) => return Ok(true),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                if attempt + 1 < LOCK_ATTEMPTS {
                    os::wait(LOCK_RETRY);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}

/// An entry at the signal name that is not ours to write through: a link,
/// a symlink, a foreign or group-writable file, or a non-regular entry.
fn is_stray(error: &FsError) -> bool {
    error.is_symlink()
        || matches!(
            error,
            FsError::NotRegular { .. }
                | FsError::LockRefused {
                    reason: LockRefusal::FileOwner
                        | LockRefusal::FilePermissions
                        | LockRefusal::LinkCount,
                    ..
                }
        )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn signal_tracks_concurrent_writers_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        // Create the signal first: the test pins writer serialisation, not
        // coincident first creation, which owners tolerate.
        std::fs::write(path.join(FILE_NAME), b"{}").unwrap();
        std::thread::scope(|threads| {
            for _ in 0..4 {
                threads.spawn(|| {
                    for _ in 0..10 {
                        bump(path, Scope::Inbox).unwrap();
                    }
                });
            }
        });
        let first = read(path).unwrap();
        assert_eq!(first.inbox, Some(40));
        std::fs::remove_file(path.join(FILE_NAME)).unwrap();
        assert!(read(path).unwrap().epoch.is_none());
        bump(path, Scope::Inbox).unwrap();
        assert_ne!(read(path).unwrap().epoch, first.epoch);
    }

    #[test]
    fn signal_bump_scopes_bumps_several_in_one_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        bump_scopes(path, &[Scope::Inbox, Scope::Board]).unwrap();
        let first = read(path).unwrap();
        assert_eq!(first.inbox, Some(1));
        assert_eq!(first.board, Some(1));
        assert_eq!(first.chair, None);
        // A second bump increments both again.
        bump_scopes(path, &[Scope::Inbox, Scope::Board]).unwrap();
        let second = read(path).unwrap();
        assert_eq!(second.inbox, Some(2));
        assert_eq!(second.board, Some(2));
    }

    #[test]
    fn signal_replaces_a_symlink_and_a_loose_mode_without_writing_through() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let target = path.join("target");
        std::fs::write(&target, b"untouched").unwrap();
        let signal = path.join(FILE_NAME);

        std::os::unix::fs::symlink(&target, &signal).unwrap();
        bump(path, Scope::Inbox).unwrap();
        let first = read(path).unwrap();
        assert_eq!(first.inbox, Some(1));
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");

        std::fs::set_permissions(&signal, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(read(path).unwrap().epoch.is_none());
        bump(path, Scope::Inbox).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        let mode = std::fs::metadata(&signal).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let replaced = read(path).unwrap();
        assert_eq!(replaced.inbox, Some(1));
        assert_ne!(replaced.epoch, first.epoch);
    }

    #[test]
    #[allow(clippy::print_stderr)]
    fn signal_replaces_a_hard_link_without_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let target = path.join("target");
        std::fs::write(&target, b"untouched").unwrap();
        let signal = path.join(FILE_NAME);
        // Some sandboxes refuse hard links with EPERM; CI runs this case.
        if let Err(error) = std::fs::hard_link(&target, &signal) {
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            eprintln!("skipped: hard links are refused here ({error})");
            return;
        }
        bump(path, Scope::Inbox).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        let replaced = read(path).unwrap();
        assert_eq!(replaced.inbox, Some(1));
        let links = std::os::unix::fs::MetadataExt::nlink(&std::fs::metadata(&signal).unwrap());
        assert_eq!(links, 1);
    }

    #[test]
    fn busy_signal_reads_as_unknown_and_bump_skips_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        bump(path, Scope::Inbox).unwrap();
        let held = File::open(path.join(FILE_NAME)).unwrap();
        held.lock().unwrap();
        assert!(read(path).unwrap().epoch.is_none());
        // The bounded wait expires and the bump is skipped, not failed.
        bump(path, Scope::Board).unwrap();
        drop(held);
        let after = read(path).unwrap();
        assert_eq!(after.inbox, Some(1));
        assert_eq!(after.board, None);
    }

    #[test]
    #[ignore = "manual write-path measurement"]
    #[allow(clippy::disallowed_methods, clippy::print_stderr)]
    fn signal_bump_cost() {
        let dir = tempfile::tempdir().unwrap();
        bump(dir.path(), Scope::Inbox).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..100 {
            bump(dir.path(), Scope::Inbox).unwrap();
        }
        eprintln!("100 signal bumps: {:?}", start.elapsed());
        let start2 = std::time::Instant::now();
        for _ in 0..100 {
            bump_scopes(dir.path(), &[Scope::Inbox, Scope::Board]).unwrap();
        }
        eprintln!("100 bump_scopes(2 scopes): {:?}", start2.elapsed());
    }
}
