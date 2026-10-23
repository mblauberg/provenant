//! One held-parent read contract for repository metadata and retained evidence.
use provenant_supervisor::fs::{FileKind, FsError, Metadata, SafeDir};
use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom};
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

pub(crate) fn same_metadata(left: &Metadata, right: &Metadata) -> bool {
    left.kind == right.kind
        && left.device == right.device
        && left.inode == right.inode
        && left.links == right.links
        && left.size == right.size
        && left.modified_ns == right.modified_ns
        && left.mode == right.mode
        && left.uid == right.uid
}

pub(crate) fn read_regular(
    parent: &dyn SafeDir,
    name: &OsStr,
    limit: u64,
) -> Result<(Vec<u8>, Metadata), ReadError> {
    let entry = parent.metadata_at(name, false)?;
    if entry.kind != FileKind::Regular || entry.links != 1 {
        return Err(ReadError::Unsupported);
    }
    if entry.size > limit {
        return Err(ReadError::TooLarge);
    }
    let mut file = parent.open_read(name)?;
    let before = file.metadata()?;
    if !same_metadata(&entry, &before) {
        return Err(ReadError::Changed);
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    (&mut *file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(ReadError::TooLarge);
    }
    let after = file.metadata()?;
    if !same_metadata(&before, &after)
        || after.size != bytes.len() as u64
        || !same_metadata(&after, &parent.metadata_at(name, false)?)
    {
        return Err(ReadError::Changed);
    }
    Ok((bytes, after))
}
