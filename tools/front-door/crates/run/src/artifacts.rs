use crate::*;
use provenant_core::{RelativePath, RunId};
use provenant_os::fs::{Directory, FileKind, PayloadTree, PublishMode};
use provenant_records::{CanonicalV1, PathWire};
use std::{ffi::OsStr, io::Read, path::Component};

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

pub(crate) fn remove_temporary_best_effort(attempt: &Directory) -> provenant_os::fs::Result<()> {
    remove_entry_tree(attempt, OsStr::new("tmp"))
}

/// Reconcile and retention GC may dispose of lock-named payloads after proving the
/// attempt terminal and unreserved. It checks worktree reservations first,
/// then holds the attempt's writer lock, which excludes read-only guardians.
/// It does not hold the worktree writer lock during deletion.
/// The exception is rooted at the literal tmp entry, never the attempt itself.
pub(crate) fn remove_unreserved_terminal_temporary(
    attempt: &Directory,
) -> provenant_os::fs::Result<()> {
    let name = OsStr::new("tmp");
    let metadata = match attempt.metadata_at(name, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.kind != FileKind::Directory {
        return attempt.unlink(name);
    }
    let tree = PayloadTree::claim(attempt, name, &metadata)?;
    remove_payload_contents(&tree)?;
    drop(tree);
    // tmp itself is not lock-named, so ordinary removal suffices.
    attempt.rmdir(name)
}

fn remove_payload_contents(directory: &PayloadTree) -> provenant_os::fs::Result<()> {
    directory.make_writable()?;
    let entries = directory
        .entries()?
        .collect::<provenant_os::fs::Result<Vec<_>>>()?;
    for entry in entries {
        if directory.metadata_at(&entry)?.kind == FileKind::Directory {
            remove_payload_contents(&directory.child(&entry)?)?;
            directory.rmdir(&entry)?;
        } else {
            directory.unlink(&entry)?;
        }
    }
    Ok(())
}

pub(crate) fn remove_entry_tree(parent: &Directory, name: &OsStr) -> provenant_os::fs::Result<()> {
    let metadata = match parent.metadata_at(name, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.kind != FileKind::Directory {
        return parent.unlink(name);
    }
    let directory = parent.open_verified_directory(name)?;
    directory.make_writable()?;
    let entries = directory
        .entries()?
        .collect::<provenant_os::fs::Result<Vec<_>>>()?;
    for entry in entries {
        remove_entry_tree(&directory, &entry)?;
    }
    parent.rmdir(name)
}

impl RunStore {
    /// Verify the original artifact before returning bounded, redacted evidence.
    /// The returned digest identifies the original artifact.
    pub fn stream_result(
        &self,
        id: provenant_core::AttemptId,
        call: &Request<'_>,
        required_verdict: Option<&str>,
    ) -> Result<provenant_records::result_capture::CapturedResult> {
        use provenant_os::exec::CaptureFilter;
        use sha2::{Digest as _, Sha256};

        let record = self.record(id, call)?;
        let artifact = record.result.as_ref().ok_or_else(|| {
            if matches!(record.state, AttemptState::Terminal { .. }) {
                RunError::NotFound
            } else {
                RunError::Nonterminal
            }
        })?;
        if artifact.bytes > RESULT_LIMIT {
            return Err(RunError::TooLarge {
                limit: RESULT_LIMIT,
            });
        }
        let dir = self.attempt_dir(record.run_id, id, false)?;
        let mut file = dir.open_read(OsStr::new("result.md"))?;
        let before = file.metadata()?;
        if before.kind != FileKind::Regular
            || before.links != 1
            || before.mode & 0o077 != 0
            || before.uid != provenant_os::fs::effective_uid()
        {
            return Err(RunError::UnsafePath);
        }
        if before.size != artifact.bytes {
            return Err(RunError::ArtifactMismatch);
        }
        let mut hash = Sha256::new();
        let mut redactor = provenant_confinement::redact::Redactor::new([])
            .map_err(|_| RunError::Validation("output redaction failed"))?;
        let mut capture = provenant_records::result_capture::ResultCapture::new(required_verdict);
        let mut buffer = [0u8; 8192];
        let mut total = 0u64;
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total = total.saturating_add(count as u64);
            if total > artifact.bytes {
                return Err(RunError::ArtifactMismatch);
            }
            hash.update(&buffer[..count]);
            let safe = redactor
                .filter(&buffer[..count])
                .map_err(|_| RunError::Validation("output redaction failed"))?;
            capture.push(&safe);
        }
        let digest = provenant_records::FileDigest::from_bytes(hash.finalize().into());
        if total != artifact.bytes
            || digest != artifact.digest
            || !before.unchanged(&file.metadata()?)
        {
            return Err(RunError::ArtifactMismatch);
        }
        let safe = redactor
            .finish()
            .map_err(|_| RunError::Validation("output redaction failed"))?;
        capture.push(&safe);
        let mut result = capture.finish();
        result.digest = digest.to_string();
        result.source_truncated =
            artifact.bytes > provenant_records::result_capture::ResultCapture::HEAD_BYTES as u64;
        Ok(result)
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
                    || m.uid != provenant_os::fs::effective_uid()
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
        || meta.uid != provenant_os::fs::effective_uid()
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
        Ok(publication) => publication.require_durable().map_err(RunError::Publication),
        Err(e) if e.is_exists() => {
            if read_sealed(dir, name, limit)?.ok_or(RunError::Conflict)? != content {
                return Err(RunError::ArtifactMismatch);
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
