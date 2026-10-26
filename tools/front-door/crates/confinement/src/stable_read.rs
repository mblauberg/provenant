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
    parent: &Directory,
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
    let bytes = parent
        .read_stable(name, limit)
        .map_err(|error| match error {
            FsError::TooLarge { .. } => ReadError::TooLarge,
            FsError::RaceRefused { .. } => ReadError::Changed,
            FsError::Io {
                operation: "read file",
                source,
                ..
            } => ReadError::Io(source),
            error => ReadError::Filesystem(error),
        })?;
    let after = parent.metadata_at(name, false)?;
    if !same_metadata(&entry, &after) || after.size != bytes.len() as u64 {
        return Err(ReadError::Changed);
    }
    Ok((bytes, after))
}
