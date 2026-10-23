//! Managed command classification. Atomic exchange/publication awaits supervisor.
use std::{fs, os::unix::{ffi::OsStrExt, fs::MetadataExt}, path::Path};
use serde::{Deserialize, Serialize};
use crate::{invalid, io, Result};

pub const MANAGED_MARKER: &[u8] = b"# Managed by scripts/install-harness; local edits to an installed copy are not preserved.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State { Absent, Existing, ManagedFile }

#[derive(Debug, PartialEq, Eq)]
struct Snapshot { identity: (u64, u64, u32, u64, i64, i64), payload: Vec<u8> }

fn identity(m: &fs::Metadata) -> (u64, u64, u32, u64, i64, i64) {
    (m.dev(), m.ino(), m.mode(), m.size(), m.mtime(), m.mtime_nsec())
}

fn snapshot(path: &Path) -> Result<Option<Snapshot>> {
    let before = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path, e)),
    };
    let read = || -> std::io::Result<(Vec<u8>, fs::Metadata)> {
        let bytes = if before.is_symlink() { fs::read_link(path)?.as_os_str().as_bytes().to_vec() }
        else if before.is_file() { fs::read(path)? } else { vec![] };
        Ok((bytes, fs::symlink_metadata(path)?))
    };
    let changed = || invalid(format!("provenant command collision={}; changed during classification", path.display()));
    let (payload, after) = read().map_err(|e| {
        if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput | std::io::ErrorKind::IsADirectory | std::io::ErrorKind::NotADirectory) { changed() } else { io(path, e) }
    })?;
    if identity(&before) != identity(&after) || (before.ctime(), before.ctime_nsec()) != (after.ctime(), after.ctime_nsec()) { return Err(changed()); }
    Ok(Some(Snapshot { identity: identity(&before), payload }))
}

pub fn classify(source: &Path, destination: &Path) -> Result<State> {
    let Some(snapshot) = snapshot(destination)? else { return Ok(State::Absent); };
    let mode = snapshot.identity.2 & 0o170000;
    let reason = if mode == 0o120000 { "remove or repoint the link to continue" }
    else if mode != 0o100000 { "remove the file to continue" }
    else {
        if snapshot.payload == fs::read(source).map_err(|e| io(source, e))? { return Ok(State::Existing); }
        // bytes.splitlines() recognises LF, CR and CRLF, including binary input.
        if snapshot.payload.split(|b| matches!(b, b'\n' | b'\r')).any(|line| line == MANAGED_MARKER) { return Ok(State::ManagedFile); }
        "remove or replace the file to continue"
    };
    Err(invalid(format!("provenant command collision={}; {reason}", destination.display())))
}

/// CLI-edge input policy, retained separately from filesystem classification.
pub fn validate_paths(source: &Path, destination: &Path) -> Result<()> {
    if !source.is_absolute() { return Err(invalid("source must be absolute")); }
    if !destination.is_absolute() { return Err(invalid("destination must be absolute")); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn binary_marker_requires_a_whole_line() {
        let dir = tempfile::tempdir().unwrap(); let source = dir.path().join("source"); let dest = dir.path().join("dest"); fs::write(&source, b"new").unwrap();
        assert_eq!(classify(&source, &dest).unwrap(), State::Absent);
        fs::write(&dest, b"new").unwrap(); assert_eq!(classify(&source, &dest).unwrap(), State::Existing);
        for newline in [b"\n".as_slice(), b"\r", b"\r\n"] {
            let bytes = [b"\0\xff".as_slice(), newline, MANAGED_MARKER, newline, b"binary"].concat(); fs::write(&dest, bytes).unwrap(); assert_eq!(classify(&source, &dest).unwrap(), State::ManagedFile);
        }
        fs::write(&dest, [b"prefix".as_slice(), MANAGED_MARKER, b"\n"].concat()).unwrap(); assert!(classify(&source, &dest).is_err());
    }
    #[test]
    fn foreign_links_are_never_owned_even_if_identical() {
        let dir = tempfile::tempdir().unwrap(); let source = dir.path().join("source"); let dest = dir.path().join("dest"); fs::write(&source, b"same").unwrap(); symlink(&source, &dest).unwrap(); assert!(classify(&source, &dest).unwrap_err().to_string().contains("remove or repoint"));
        fs::remove_file(source).unwrap(); assert!(classify(&dir.path().join("source"), &dest).is_err());
    }
    #[test]
    fn directories_and_relative_paths_are_refused() {
        let dir = tempfile::tempdir().unwrap(); assert!(classify(&dir.path().join("source"), dir.path()).unwrap_err().to_string().contains("remove the file"));
        assert_eq!(validate_paths(Path::new("source"), dir.path()).unwrap_err().to_string(), "source must be absolute");
    }
    #[test]
    fn python_classification_differential() {
        let dir = tempfile::tempdir().unwrap(); let source = dir.path().join("source"); let dest = dir.path().join("dest"); fs::write(&source, b"new").unwrap();
        for bytes in [b"new".to_vec(), [b"\xff\n".as_slice(), MANAGED_MARKER, b"\n"].concat(), b"foreign".to_vec()] {
            fs::write(&dest, bytes).unwrap();
            let Some(output) = crate::oracle::python("scripts/install-provenant-command.py", "try: print(m.classify(p.parent/'source', p))\nexcept m.Collision: print('collision')", &dest) else { return; };
            let actual = match classify(&source, &dest) { Ok(State::Existing) => "existing", Ok(State::ManagedFile) => "managed-file", Ok(State::Absent) => "absent", Err(_) => "collision" };
            assert_eq!(String::from_utf8(output).unwrap().trim(), actual);
        }
    }
}
