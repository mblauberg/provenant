//! The provider transcript an attempt left in its retained provider state.
//! The run records a pointer (adapter and session); the file stays with the
//! provider and is read in place, never copied.
use provenant_os::fs::{Directory, FileKind};
use std::{
    ffi::{OsStr, OsString},
    io::{Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// Largest tail of a transcript one read returns.
pub(crate) const WINDOW: u64 = 4 << 20;
const DEPTH: u8 = 8;
const ENTRIES: u32 = 20_000;

/// The `.jsonl` file named for `session` under the attempt's provider state,
/// as a directory handle, its name, and the path relative to the attempt.
/// Bounded, never follows a link and stays on one device.
pub(crate) fn find(attempt: &Directory, session: &str) -> Option<(Directory, OsString, PathBuf)> {
    fn walk(
        dir: &Directory,
        session: &str,
        depth: u8,
        device: u64,
        budget: &mut u32,
        path: &Path,
    ) -> Option<(Directory, OsString, PathBuf)> {
        let names: Vec<OsString> = dir.entries().ok()?.map_while(Result::ok).collect();
        for name in names {
            *budget = budget.checked_sub(1)?;
            let meta = dir.metadata_at(&name, false).ok()?;
            let text = name.to_string_lossy();
            if meta.kind == FileKind::Regular && text.ends_with(".jsonl") && text.contains(session)
            {
                return Some((dir.try_clone().ok()?, name.clone(), path.join(&name)));
            }
            if depth > 0 && meta.kind == FileKind::Directory && meta.device == device {
                let child = dir.open_verified_directory(&name).ok()?;
                if let Some(found) = walk(
                    &child,
                    session,
                    depth - 1,
                    device,
                    budget,
                    &path.join(&name),
                ) {
                    return Some(found);
                }
            }
        }
        None
    }
    if session.is_empty() {
        return None;
    }
    let state = attempt
        .open_verified_directory(OsStr::new("provider-state"))
        .ok()?;
    let device = state.metadata().ok()?.device;
    walk(
        &state,
        session,
        DEPTH,
        device,
        &mut { ENTRIES },
        Path::new("provider-state"),
    )
}

/// Where the transcript of `session` sits under an attempt directory.
pub fn locate(attempt: &Path, session: &str) -> Option<PathBuf> {
    let attempt = Directory::open_anchor(attempt).ok()?;
    find(&attempt, session).map(|(_, _, path)| path)
}

/// The last `WINDOW` bytes of the transcript, starting at a line boundary, and
/// whether that is the whole file.
pub(crate) fn tail(dir: &Directory, name: &OsStr) -> Option<(Vec<u8>, bool)> {
    let mut file = dir.open_read(name).ok()?;
    let meta = file.metadata().ok()?;
    if meta.kind != FileKind::Regular {
        return None;
    }
    let clipped = meta.size > WINDOW;
    if clipped {
        file.seek(SeekFrom::Start(meta.size - WINDOW)).ok()?;
    }
    let mut bytes = file.read_bounded(WINDOW).ok()?;
    if clipped {
        let start = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        bytes.drain(..start);
    }
    Some((bytes, !clipped))
}
