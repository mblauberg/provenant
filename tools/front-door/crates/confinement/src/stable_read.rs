//! One held-parent read contract for repository metadata and retained evidence.
use provenant_os::fs::{Directory, FileKind, FsError, Metadata};
use std::ffi::OsStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum ReadError {
    #[error("not a single-link regular file")]
    Unsupported,
    #[error("file exceeds its byte bound")]
    TooLarge,
    #[error("file changed during read")]
    Changed,
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) fn read_regular(
    parent: &Directory,
    name: &OsStr,
    limit: u64,
) -> Result<(Vec<u8>, Metadata), ReadError> {
    let entry = parent.metadata_at(name, false)?;
    verify_entry(entry, limit)?;
    let bytes = parent
        .read_stable(name, limit)
        .map_err(|error| match error {
            FsError::TooLarge { .. } => ReadError::TooLarge,
            FsError::RaceRefused { .. } => ReadError::Changed,
            FsError::ReadIo { source, .. } => ReadError::Io(source),
            error => ReadError::Filesystem(error),
        })?;
    let after = parent.metadata_at(name, false)?;
    if !entry.unchanged(&after) || after.size != bytes.len() as u64 {
        return Err(ReadError::Changed);
    }
    Ok((bytes, after))
}

fn verify_entry(entry: Metadata, limit: u64) -> Result<(), ReadError> {
    if entry.kind != FileKind::Regular || entry.links > 1 {
        return Err(ReadError::Unsupported);
    }
    // A pathname lookup can resolve an inode just before an atomic replacement
    // unlinks it. Linux may then return its metadata with no remaining links.
    // Retry the changed snapshot; never accept the unlinked inode as stable.
    if entry.links == 0 {
        return Err(ReadError::Changed);
    }
    if entry.size > limit {
        return Err(ReadError::TooLarge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn replaced_regular_file_is_changed_but_hard_links_and_wrong_kinds_are_unsupported()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config");
        fs::write(&path, b"[core]\n bare = false\n")?;
        let file = fs::File::open(&path)?;
        assert!(verify_entry(Metadata::from_std(file.metadata()?), 1024).is_ok());
        fs::hard_link(&path, temp.path().join("alias"))?;
        assert!(matches!(
            verify_entry(Metadata::from_std(file.metadata()?), 1024),
            Err(ReadError::Unsupported)
        ));
        fs::remove_file(temp.path().join("alias"))?;
        // Model the metadata observation after Git has replaced the resolved
        // config inode, while a lookup still holds a reference to the old one.
        fs::write(temp.path().join("config.lock"), b"[core]\n bare = false\n")?;
        fs::rename(temp.path().join("config.lock"), &path)?;
        assert!(matches!(
            verify_entry(Metadata::from_std(file.metadata()?), 1024),
            Err(ReadError::Changed)
        ));
        let parent = Directory::open_anchor(temp.path())?;
        assert_eq!(
            read_regular(&parent, OsStr::new("config"), 1024)?.0,
            b"[core]\n bare = false\n"
        );
        assert!(matches!(
            verify_entry(Metadata::from_std(fs::metadata(temp.path())?), 1024),
            Err(ReadError::Unsupported)
        ));
        std::os::unix::fs::symlink(&path, temp.path().join("link"))?;
        assert!(matches!(
            read_regular(&parent, OsStr::new("link"), 1024),
            Err(ReadError::Unsupported)
        ));
        Ok(())
    }
}
