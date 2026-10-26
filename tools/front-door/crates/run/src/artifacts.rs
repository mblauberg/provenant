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
/// attempt terminal and unreserved. It checks worktree reservations first,
/// then holds the attempt's writer lock, which excludes read-only guardians.
/// It does not hold the worktree writer lock during deletion.
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
    /// This store's path below the trusted host root: empty for a top-level
    /// store, `children/<AttemptId>/...` for a lane's child store.
    fn control_key(&self) -> Result<std::path::PathBuf> {
        let key = self
            .context
            .state_root
            .as_path()
            .strip_prefix(&self.writer_registry)
            .map_err(|_| RunError::UnsafePath)?;
        if key
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(RunError::UnsafePath);
        }
        Ok(key.to_owned())
    }
    /// A lane's child store, whose attempts stop through host-owned control
    /// directories rather than their lane-writable attempt directories.
    pub(crate) fn child_store(&self) -> bool {
        self.control_key()
            .is_ok_and(|key| !key.as_os_str().is_empty())
    }
    /// `<host state root>/control/<store path below it>`, which mirrors the
    /// store so lane-chosen attempt IDs in different stores never share a
    /// name. No lane profile grants access there.
    pub(crate) fn control_parent(&self, create: bool) -> Result<Directory> {
        let root = Directory::open_anchor(&self.writer_registry)?;
        db::private(&root)?;
        let mut directory = subdir(&root, OsStr::new("control"), create)?;
        for part in self.control_key()?.components() {
            directory = subdir(&directory, part.as_os_str(), create)?;
        }
        Ok(directory)
    }
    /// A child's control directory, found by name.
    pub(crate) fn child_control(&self, attempt: provenant_core::AttemptId) -> Result<Directory> {
        subdir(
            &self.control_parent(false)?,
            OsStr::new(&attempt.to_string()),
            false,
        )
    }
    /// Where a stop is written when no handle was held at hand-off: a
    /// top-level attempt's directory, or a child's control directory. A
    /// confined lane cannot open the latter, so its request lands in the
    /// child's attempt directory and the parent's host relays it.
    pub(crate) fn stop_control(
        &self,
        attempt: &Directory,
        id: provenant_core::AttemptId,
    ) -> Result<Directory> {
        if self.child_store()
            && let Ok(control) = self.child_control(id)
        {
            return Ok(control);
        }
        Ok(attempt.try_clone()?)
    }
    /// Remove a child's control directory once its terminal has committed.
    /// With `inherited`, only the directory the guardian inherited goes.
    pub(crate) fn remove_control(
        &self,
        id: provenant_core::AttemptId,
        inherited: Option<&Directory>,
    ) -> Result<()> {
        if !self.child_store() {
            return Ok(());
        }
        let parent = self.control_parent(false)?;
        let name = id.to_string();
        let name = OsStr::new(&name);
        if let Some(inherited) = inherited
            && !parent
                .metadata_at(name, false)?
                .same_identity(&inherited.metadata()?)
        {
            return Ok(());
        }
        remove_entry_tree(&parent, name, TemporaryLocks::Protected)?;
        parent.sync()?;
        Ok(())
    }
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
