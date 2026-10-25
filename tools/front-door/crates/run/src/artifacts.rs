use crate::*;
use provenant_core::{RelativePath, RunId};
use provenant_records::{CanonicalV1, PathWire};
use provenant_supervisor::fs::{Directory, FileKind, Publication, PublishMode};
use std::{ffi::OsStr, path::Component};

pub(crate) fn subdir(parent: &Directory, name: &OsStr, create: bool) -> Result<Directory> {
    if create {
        match parent.mkdir(name, 0o700) {
            Ok(()) => parent.sync()?,
            Err(e) if e.is_exists() => {}
            Err(e) => return Err(e.into()),
        }
    }
    let dir = parent.open_verified_directory(name)?;
    db::private(&dir)?;
    Ok(dir)
}

pub(crate) fn remove_temporary_best_effort(
    attempt: &Directory,
) -> provenant_supervisor::fs::Result<()> {
    remove_entry_tree(attempt, OsStr::new("tmp"), TemporaryLocks::Protected)
}

/// Reconcile alone may dispose of lock-named payloads after proving the
/// attempt terminal and unreserved, while holding its writer exclusion lock.
/// The exception is rooted at the literal tmp entry, never the attempt itself.
pub(crate) fn remove_unreserved_terminal_temporary(
    attempt: &Directory,
) -> provenant_supervisor::fs::Result<()> {
    remove_entry_tree(attempt, OsStr::new("tmp"), TemporaryLocks::Disposable)
}

#[derive(Clone, Copy)]
enum TemporaryLocks {
    Protected,
    Disposable,
}

fn remove_entry_tree(
    parent: &Directory,
    name: &OsStr,
    locks: TemporaryLocks,
) -> provenant_supervisor::fs::Result<()> {
    let metadata = match parent.metadata_at(name, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.kind != FileKind::Directory {
        return match locks {
            TemporaryLocks::Protected => parent.unlink(name),
            TemporaryLocks::Disposable => parent.unlink_payload(name),
        };
    }
    let directory = parent.open_verified_directory(name)?;
    directory.make_writable()?;
    let entries = directory
        .entries()?
        .collect::<provenant_supervisor::fs::Result<Vec<_>>>()?;
    for entry in entries {
        remove_entry_tree(&directory, &entry, locks)?;
    }
    match locks {
        TemporaryLocks::Protected => parent.rmdir(name),
        TemporaryLocks::Disposable => parent.rmdir_payload(name),
    }
}

impl RunStore {
    pub(crate) fn run_dir(&self, run: RunId, create: bool) -> Result<Directory> {
        let runs = subdir(&self.root, OsStr::new("runs"), create)?;
        subdir(&runs, OsStr::new(&run.to_string()), create)
    }
    pub(crate) fn attempt_dir(
        &self,
        run: RunId,
        attempt: provenant_core::AttemptId,
        create: bool,
    ) -> Result<Directory> {
        let run = self.run_dir(run, create)?;
        let attempts = subdir(&run, OsStr::new("attempts"), create)?;
        subdir(&attempts, OsStr::new(&attempt.to_string()), create)
    }
    pub(crate) fn artifact_bytes(
        &self,
        run: RunId,
        artifact: &ArtifactRecord,
        limit: u64,
    ) -> Result<Vec<u8>> {
        if artifact.bytes > limit {
            return Err(RunError::TooLarge { limit });
        }
        let mut dir = self.run_dir(run, false)?;
        let path = artifact.path.to_relative_path()?;
        let mut parts = path.as_path().components().peekable();
        while let Some(Component::Normal(name)) = parts.next() {
            if parts.peek().is_some() {
                dir = subdir(&dir, name, false)?;
            } else {
                let m = dir.metadata_at(name, false)?;
                if m.kind != FileKind::Regular
                    || m.links != 1
                    || m.mode & 0o077 != 0
                    || m.uid != provenant_supervisor::fs::effective_uid()
                {
                    return Err(RunError::UnsafePath);
                }
                let bytes = dir.read_bounded(name, limit)?;
                if bytes.len() as u64 != artifact.bytes
                    || CanonicalV1::file_digest(&bytes) != artifact.digest
                {
                    return Err(RunError::ArtifactMismatch);
                }
                return Ok(bytes);
            }
        }
        Err(RunError::UnsafePath)
    }
}
pub(crate) fn artifact_path(attempt: provenant_core::AttemptId, name: &str) -> Result<PathWire> {
    let path = RelativePath::parse(format!("attempts/{attempt}/{name}"))
        .map_err(|_| RunError::UnsafePath)?;
    Ok(PathWire::from_relative(&path)?)
}
pub(crate) fn read_sealed(dir: &Directory, name: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    let mut file = match dir.open_read(OsStr::new(name)) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let meta = file.metadata()?;
    if meta.kind != FileKind::Regular
        || meta.links != 1
        || meta.mode & 0o077 != 0
        || meta.uid != provenant_supervisor::fs::effective_uid()
    {
        return Err(RunError::UnsafePath);
    }
    let bytes = file.read_bounded(limit)?;
    file.sync()?;
    dir.sync()?;
    Ok(Some(bytes))
}
pub(crate) fn seal(dir: &Directory, name: &str, content: &[u8], limit: u64) -> Result<()> {
    if content.len() as u64 > limit {
        return Err(RunError::TooLarge { limit });
    }
    match dir.atomic_write(OsStr::new(name), ".run-", content, PublishMode::NoReplace) {
        Ok(Publication::Durable) => Ok(()),
        Ok(Publication::VisibleDurabilityUnconfirmed { sync_error }) => {
            Err(RunError::Publication(sync_error))
        }
        Err(e) if e.is_exists() => {
            if read_sealed(dir, name, limit)?.ok_or(RunError::Conflict)? != content {
                return Err(RunError::ArtifactMismatch);
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
