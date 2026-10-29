//! Owner-issued change signal, stored in one locked, private state file.
use std::ffi::OsStr;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::fs::{Directory, LockMode};
use crate::sys::fs as os;
pub use provenant_core::changes::{FILE_NAME, Generations, Scope};

/// A bounded, non-blocking read. A busy or missing signal is unknown.
pub fn read(state_dir: &Path) -> io::Result<Generations> {
    let dir = match Directory::open_anchor(state_dir) {
        Ok(dir) => dir,
        Err(e) if e.is_not_found() => return Ok(Generations::default()),
        Err(e) => return Err(e.into()),
    };
    let name = OsStr::new(FILE_NAME);
    let file = match dir.signal_file(name, false) {
        Ok(file) => file,
        Err(e) if e.is_not_found() => return Ok(Generations::default()),
        Err(e) => return Err(e.into()),
    };
    match os::try_flock(&file, LockMode::Shared) {
        Ok(()) => (),
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

/// Bump after an owner commit, serialising writers on the signal itself.
pub fn bump(state_dir: &Path, scope: Scope) -> io::Result<()> {
    let dir = Directory::open_anchor(state_dir).map_err(io::Error::from)?;
    let name = OsStr::new(FILE_NAME);
    let mut file = dir.signal_file(name, true).map_err(io::Error::from)?;
    file.lock()?;
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
    let data = serde_json::to_vec(&generations).map_err(io::Error::other)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&data)?;
    file.set_len(data.len() as u64)?;
    file.sync_data()
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
    fn signal_refuses_links_and_busy_read_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"untouched").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(FILE_NAME)).unwrap();
        assert!(bump(dir.path(), Scope::Inbox).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        std::fs::remove_file(dir.path().join(FILE_NAME)).unwrap();
        std::fs::hard_link(&target, dir.path().join(FILE_NAME)).unwrap();
        assert!(bump(dir.path(), Scope::Inbox).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        std::fs::remove_file(dir.path().join(FILE_NAME)).unwrap();
        bump(dir.path(), Scope::Inbox).unwrap();
        let file = std::fs::File::open(dir.path().join(FILE_NAME)).unwrap();
        file.lock().unwrap();
        assert!(read(dir.path()).unwrap().epoch.is_none());
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
    }
}
