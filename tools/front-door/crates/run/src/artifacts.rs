use crate::*;
use provenant_core::{RelativePath, RunId};
use provenant_os::fs::{Directory, FileKind, Metadata, PayloadTree, PublishMode};
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
        if let Some(check) = PathCheck::file(before) {
            return Err(RunError::unsafe_entry(&dir, OsStr::new("result.md"), check));
        }
        if before.size != artifact.bytes {
            return Err(RunError::artifact_mismatch(
                ArtifactCheck::RecordedLength,
                "result.md",
                Some(ArtifactValue::Length(artifact.bytes)),
                Some(ArtifactValue::Length(before.size)),
            ));
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
                return Err(RunError::artifact_mismatch(
                    ArtifactCheck::Stream,
                    "result.md",
                    Some(ArtifactValue::Length(artifact.bytes)),
                    Some(ArtifactValue::Length(total)),
                ));
            }
            hash.update(&buffer[..count]);
            let safe = redactor
                .filter(&buffer[..count])
                .map_err(|_| RunError::Validation("output redaction failed"))?;
            capture.push(&safe);
        }
        let digest = provenant_records::FileDigest::from_bytes(hash.finalize().into());
        if total != artifact.bytes {
            return Err(RunError::artifact_mismatch(
                ArtifactCheck::Stream,
                "result.md",
                Some(ArtifactValue::Length(artifact.bytes)),
                Some(ArtifactValue::Length(total)),
            ));
        }
        if digest != artifact.digest {
            return Err(RunError::artifact_mismatch(
                ArtifactCheck::RecordedDigest,
                "result.md",
                Some(ArtifactValue::digest(artifact.digest)),
                Some(ArtifactValue::digest(digest)),
            ));
        }
        if !before.unchanged(&file.metadata()?) {
            return Err(RunError::artifact_mismatch(
                ArtifactCheck::Stream,
                "result.md",
                None,
                None,
            ));
        }
        let safe = redactor
            .finish()
            .map_err(|_| RunError::Validation("output redaction failed"))?;
        capture.push(&safe);
        let mut result = capture.finish();
        // Graph continuations consume decoded handoff data. Byte redaction
        // alone cannot see token boundaries hidden behind JSON escapes.
        result.handoff = result.handoff.and_then(|handoff| {
            handoff
                .map(|handoff| {
                    crate::completion::sanitise_handoff(&handoff)
                        .map_err(|_| "handoff sanitisation failed")
                })
                .transpose()
        });
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
                if let Some(check) = PathCheck::file(dir.metadata_at(name, false)?) {
                    return Err(RunError::unsafe_entry(&dir, name, check));
                }
                let bytes = dir.read_bounded(name, limit)?;
                let length = bytes.len() as u64;
                if length != artifact.bytes {
                    return Err(RunError::artifact_mismatch(
                        ArtifactCheck::RecordedLength,
                        path.as_path().display().to_string(),
                        Some(ArtifactValue::Length(artifact.bytes)),
                        Some(ArtifactValue::Length(length)),
                    ));
                }
                let digest = CanonicalV1::file_digest(&bytes);
                if digest != artifact.digest {
                    return Err(RunError::artifact_mismatch(
                        ArtifactCheck::RecordedDigest,
                        path.as_path().display().to_string(),
                        Some(ArtifactValue::digest(artifact.digest)),
                        Some(ArtifactValue::digest(digest)),
                    ));
                }
                return Ok(bytes);
            }
        }
        Err(RunError::unsafe_path(
            path.as_path(),
            PathCheck::Containment,
        ))
    }
}
pub(crate) fn artifact_path(attempt: provenant_core::AttemptId, name: &str) -> Result<PathWire> {
    let path = RelativePath::parse(format!("attempts/{attempt}/{name}")).map_err(|_| {
        RunError::unsafe_path(format!("attempts/{attempt}/{name}"), PathCheck::Containment)
    })?;
    Ok(PathWire::from_relative(&path)?)
}
/// Opens of a sealed name that concurrent replacements may retire in a row.
const REPLACED_OPENS: usize = 4;
/// Counts durable reads on the calling thread so tests can prove which record
/// path a consumer takes without relying on timing.
#[cfg(test)]
pub(crate) mod sync_probe {
    use std::cell::Cell;
    thread_local! {
        static DURABLE_SYNCS: Cell<usize> = const { Cell::new(0) };
    }
    pub(crate) fn reset() {
        DURABLE_SYNCS.with(|count| count.set(0));
    }
    pub(crate) fn count() -> usize {
        DURABLE_SYNCS.with(Cell::get)
    }
    pub(crate) fn record() {
        DURABLE_SYNCS.with(|count| count.set(count.get() + 1));
    }
}
pub(crate) fn read_sealed(dir: &Directory, name: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    read_sealed_observing(dir, name, limit, || {})
}
/// A read-only observation of a sealed name. It skips the two syncs that make
/// [`read_sealed`] durable, so callers report what is published without paying
/// an `F_FULLFSYNC` per read. Never use the bytes to decide a write.
pub(crate) fn read_sealed_observed(
    dir: &Directory,
    name: &str,
    limit: u64,
) -> Result<Option<Vec<u8>>> {
    read_sealed_with(dir, name, limit, false, || {})
}
/// A checkpoint such as process.json is replaced by rename while readers poll it.
/// A handle opened just before the rename then reports no links. That is a
/// lifecycle transition, not a hard-link attack: discard the handle unread and
/// resolve the name again, so privacy is judged on the entry now published.
/// A name that vanished meanwhile is absent. `opened` runs after each open.
pub(crate) fn read_sealed_observing(
    dir: &Directory,
    name: &str,
    limit: u64,
    opened: impl FnMut(),
) -> Result<Option<Vec<u8>>> {
    read_sealed_with(dir, name, limit, true, opened)
}
fn read_sealed_with(
    dir: &Directory,
    name: &str,
    limit: u64,
    durable: bool,
    mut opened: impl FnMut(),
) -> Result<Option<Vec<u8>>> {
    let mut retired = 0;
    loop {
        let mut file = match dir.open_read(OsStr::new(name)) {
            Ok(file) => file,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        opened();
        let meta = file.metadata()?;
        let unlinked = meta.links == 0;
        let judged = Metadata {
            links: meta.links.max(1),
            ..meta
        };
        if let Some(check) = PathCheck::file(judged) {
            return Err(RunError::unsafe_entry(dir, OsStr::new(name), check));
        }
        if unlinked {
            retired += 1;
            if retired >= REPLACED_OPENS {
                return Err(RunError::unsafe_entry(
                    dir,
                    OsStr::new(name),
                    PathCheck::Links,
                ));
            }
            continue;
        }
        let bytes = file.read_bounded(limit)?;
        if durable {
            file.sync()?;
            dir.sync()?;
            #[cfg(test)]
            sync_probe::record();
        }
        return Ok(Some(bytes));
    }
}
pub(crate) fn seal(dir: &Directory, name: &str, content: &[u8], limit: u64) -> Result<()> {
    if content.len() as u64 > limit {
        return Err(RunError::TooLarge { limit });
    }
    match dir.atomic_write(OsStr::new(name), ".run-", content, PublishMode::NoReplace) {
        Ok(publication) => publication.require_durable().map_err(RunError::Publication),
        Err(e) if e.is_exists() => {
            let sealed = read_sealed(dir, name, limit)?.ok_or(RunError::Conflict)?;
            if sealed != content {
                return Err(RunError::artifact_mismatch(
                    ArtifactCheck::SealConflict,
                    name,
                    Some(ArtifactValue::digest(CanonicalV1::file_digest(&sealed))),
                    Some(ArtifactValue::digest(CanonicalV1::file_digest(content))),
                ));
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn checkpoint(dir: &Directory, bytes: &[u8]) -> TestResult {
        dir.atomic_write(
            OsStr::new("process.json"),
            ".checkpoint-",
            bytes,
            PublishMode::Replace,
        )?
        .require_durable()?;
        Ok(())
    }

    #[test]
    fn read_sealed_follows_a_checkpoint_replaced_after_open() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        checkpoint(&dir, b"old")?;
        let mut opens = 0;
        let read = read_sealed_observing(&dir, "process.json", 1024, || {
            opens += 1;
            if opens == 1 {
                // The guardian's next periodic checkpoint lands between the
                // reader's open and its fstat, leaving the handle unlinked.
                checkpoint(&dir, b"new").expect("replace checkpoint");
            }
        })?;
        assert_eq!(read.as_deref(), Some(&b"new"[..]));
        assert_eq!(opens, 2);
        Ok(())
    }

    #[test]
    fn observed_read_follows_a_checkpoint_replaced_after_open() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        checkpoint(&dir, b"old")?;
        let mut opens = 0;
        let read = read_sealed_with(&dir, "process.json", 1024, false, || {
            opens += 1;
            if opens == 1 {
                checkpoint(&dir, b"new").expect("replace checkpoint");
            }
        })?;
        assert_eq!(read.as_deref(), Some(&b"new"[..]));
        assert_eq!(opens, 2);
        assert_eq!(read, read_sealed(&dir, "process.json", 1024)?);
        Ok(())
    }

    #[test]
    fn read_sealed_treats_a_name_retired_after_open_as_absent() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        checkpoint(&dir, b"old")?;
        let read = read_sealed_observing(&dir, "process.json", 1024, || {
            let _ = dir.unlink(OsStr::new("process.json"));
        })?;
        assert_eq!(read, None);
        Ok(())
    }

    #[test]
    fn read_sealed_gives_up_on_a_name_replaced_on_every_open() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        checkpoint(&dir, b"old")?;
        let mut opens = 0;
        let read = read_sealed_observing(&dir, "process.json", 1024, || {
            opens += 1;
            checkpoint(&dir, b"again").expect("replace checkpoint");
        });
        assert!(matches!(
            read,
            Err(RunError::UnsafePath {
                check: PathCheck::Links,
                ..
            })
        ));
        assert_eq!(opens, REPLACED_OPENS);
        Ok(())
    }

    #[test]
    fn read_sealed_still_refuses_unsafe_entries_and_names_them() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        checkpoint(&dir, b"x")?;
        let meta = dir.metadata_at(OsStr::new("process.json"), false)?;
        assert_eq!(PathCheck::file(meta), None);
        assert_eq!(
            PathCheck::file(Metadata { links: 2, ..meta }),
            Some(PathCheck::Links)
        );
        assert_eq!(
            PathCheck::file(Metadata {
                uid: meta.uid.wrapping_add(1),
                ..meta
            }),
            Some(PathCheck::Owner)
        );
        assert_eq!(
            PathCheck::file(Metadata {
                kind: FileKind::Symlink,
                ..meta
            }),
            Some(PathCheck::Symlink)
        );
        std::fs::set_permissions(
            temp.path().join("process.json"),
            std::fs::Permissions::from_mode(0o644),
        )?;
        let error = read_sealed(&dir, "process.json", 1024).expect_err("group-readable");
        assert!(matches!(
            error,
            RunError::UnsafePath {
                check: PathCheck::Mode,
                ..
            }
        ));
        assert_eq!(
            error.to_string(),
            "run-store path process.json is not private or contained: mode"
        );
        Ok(())
    }

    #[test]
    fn seal_conflict_names_the_sealed_and_proposed_digests() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        seal(&dir, "result.md", b"hello", 1024)?;
        seal(&dir, "result.md", b"hello", 1024)?;
        assert_eq!(
            crate::tests::artifact_provenance(seal(&dir, "result.md", b"hellp", 1024)),
            ArtifactProvenance {
                check: ArtifactCheck::SealConflict,
                artifact: "result.md".into(),
                expected: Some(crate::tests::digest_prefix(CanonicalV1::file_digest(
                    b"hello"
                ))),
                actual: Some(crate::tests::digest_prefix(CanonicalV1::file_digest(
                    b"hellp"
                ))),
            }
        );
        Ok(())
    }

    #[test]
    fn artifact_read_names_the_recorded_length_or_digest_that_failed() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = crate::tests::context(temp.path())?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        let id = run.attempts[0].attempt_id;
        store.create(&run, &call)?;
        seal(
            &store.attempt_dir(run.run_id, id, false)?,
            "result.md",
            b"hello",
            1024,
        )?;
        let path = artifact_path(id, "result.md")?;
        let record = |bytes: u64, digest: provenant_records::FileDigest| ArtifactRecord {
            extra: Default::default(),
            path: path.clone(),
            digest,
            bytes,
            complete: true,
            redacted: false,
        };
        let honest = record(5, CanonicalV1::file_digest(b"hello"));
        assert_eq!(store.artifact_bytes(run.run_id, &honest, 1024)?, b"hello");
        let artifact = format!("attempts/{id}/result.md");
        assert_eq!(
            crate::tests::artifact_provenance(store.artifact_bytes(
                run.run_id,
                &record(6, CanonicalV1::file_digest(b"hello")),
                1024
            )),
            ArtifactProvenance {
                check: ArtifactCheck::RecordedLength,
                artifact: artifact.clone(),
                expected: Some(ArtifactValue::Length(6)),
                actual: Some(ArtifactValue::Length(5)),
            }
        );
        assert_eq!(
            crate::tests::artifact_provenance(store.artifact_bytes(
                run.run_id,
                &record(5, CanonicalV1::file_digest(b"hellp")),
                1024
            )),
            ArtifactProvenance {
                check: ArtifactCheck::RecordedDigest,
                artifact,
                expected: Some(crate::tests::digest_prefix(CanonicalV1::file_digest(
                    b"hellp"
                ))),
                actual: Some(crate::tests::digest_prefix(CanonicalV1::file_digest(
                    b"hello"
                ))),
            }
        );
        Ok(())
    }

    #[test]
    fn unsafe_directory_is_named_relative_to_the_state_root() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let attempt = run.attempts[0].attempt_id;
        let relative = Path::new("runs")
            .join(run.run_id.to_string())
            .join("attempts")
            .join(attempt.to_string());
        std::fs::set_permissions(
            context.state_root.as_path().join(&relative),
            std::fs::Permissions::from_mode(0o755),
        )?;
        let error = store
            .attempt_dir(run.run_id, attempt, false)
            .expect_err("group-readable attempt directory");
        assert!(
            matches!(
                &error,
                RunError::UnsafePath { path, check: PathCheck::Mode } if path == &relative
            ),
            "{error:?}"
        );
        Ok(())
    }
}
