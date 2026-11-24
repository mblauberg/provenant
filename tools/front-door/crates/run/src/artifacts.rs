use crate::*;
use provenant_core::RelativePath;
use provenant_os::fs::{Directory, FileHandle, FileKind, Metadata, PayloadTree, PublishMode};
use provenant_records::{AttemptFile, CanonicalV1, PathWire, handoff::HandoffError};
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

/// Reconcile and retention GC may dispose of lock-named payloads after proving the
/// attempt terminal and unreserved. It checks worktree reservations first,
/// then holds the attempt's writer lock, which excludes read-only guardians.
/// It does not hold the worktree writer lock during deletion.
/// The exception is rooted at the literal tmp entry, never the attempt itself.
pub(crate) fn remove_unreserved_terminal_temporary(
    attempt: &Directory,
    keep_going: &dyn Fn() -> bool,
    reclaimed: &mut u64,
) -> provenant_os::fs::Result<u64> {
    if !keep_going() {
        return Err(provenant_os::fs::FsError::Cancelled {
            path: attempt.anchored_path().join("tmp"),
        });
    }
    crate::short_temporary::remove(attempt);
    let name = OsStr::new("tmp");
    let metadata = match attempt.metadata_at(name, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Ok(0),
        Err(error) => return Err(error),
    };
    if metadata.kind != FileKind::Directory {
        attempt.unlink(name)?;
        *reclaimed = reclaimed.saturating_add(metadata.allocated);
        return Ok(metadata.allocated);
    }
    attempt.make_owner_accessible_at(name)?;
    let metadata = attempt.metadata_at(name, false)?;
    let tree = PayloadTree::claim(attempt, name, &metadata)?;
    let bytes = metadata
        .allocated
        .saturating_add(remove_payload_contents(&tree, keep_going, reclaimed)?);
    drop(tree);
    if !attempt.metadata_at(name, false)?.same_identity(&metadata) {
        return Err(provenant_os::fs::FsError::PayloadMismatch { name: name.into() });
    }
    // tmp itself is not lock-named, so ordinary removal suffices.
    attempt.rmdir(name)?;
    *reclaimed = reclaimed.saturating_add(metadata.allocated);
    Ok(bytes)
}

/// Dispose only a claimed regenerable payload, checking the caller's budget
/// between entries. An explicit stack supports deep trees without call-stack
/// growth; completed unlinks are the resume state for an interrupted pass.
pub(crate) fn remove_payload_contents(
    directory: &PayloadTree,
    keep_going: &dyn Fn() -> bool,
    reclaimed: &mut u64,
) -> provenant_os::fs::Result<u64> {
    struct Frame {
        tree: Option<PayloadTree>,
        name: Option<std::ffi::OsString>,
        allocated: u64,
        pending: std::collections::VecDeque<std::ffi::OsString>,
    }
    let mut stack = vec![Frame {
        tree: None,
        name: None,
        allocated: 0,
        pending: Default::default(),
    }];
    let mut bytes = 0_u64;
    if directory.metadata()?.uid == provenant_os::fs::effective_uid() {
        directory.make_writable()?;
    }
    while let Some(frame) = stack.last_mut() {
        let current = frame.tree.as_ref().unwrap_or(directory);
        if !keep_going() {
            return Err(provenant_os::fs::FsError::Cancelled {
                path: current.directory().anchored_path(),
            });
        }
        if frame.pending.is_empty() {
            for entry in current.entries()?.take(64) {
                if !keep_going() {
                    return Err(provenant_os::fs::FsError::Cancelled {
                        path: current.directory().anchored_path(),
                    });
                }
                frame.pending.push_back(entry?);
            }
        }
        if let Some(entry) = frame.pending.pop_front() {
            let metadata = current.metadata_at(&entry)?;
            bytes = bytes.saturating_add(metadata.allocated);
            if metadata.kind == FileKind::Directory {
                current.directory().make_owner_accessible_at(&entry)?;
                let child = current.child(&entry)?;
                if child.metadata()?.uid == provenant_os::fs::effective_uid() {
                    child.make_writable()?;
                }
                stack.push(Frame {
                    tree: Some(child),
                    name: Some(entry),
                    allocated: metadata.allocated,
                    pending: Default::default(),
                });
            } else {
                current.unlink(&entry)?;
                *reclaimed = reclaimed.saturating_add(metadata.allocated);
            }
            continue;
        }
        let Some(done) = stack.pop() else {
            break;
        };
        if let Some(name) = done.name {
            drop(done.tree);
            let parent = stack
                .last()
                .and_then(|frame| frame.tree.as_ref())
                .unwrap_or(directory);
            parent.rmdir(&name)?;
            *reclaimed = reclaimed.saturating_add(done.allocated);
        }
    }
    Ok(bytes)
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
    parent.make_owner_accessible_at(name)?;
    let directory = parent.open_verified_directory(name)?;
    if directory.metadata()?.uid == provenant_os::fs::effective_uid() {
        directory.make_writable()?;
    }
    let entries = directory
        .entries()?
        .collect::<provenant_os::fs::Result<Vec<_>>>()?;
    for entry in entries {
        remove_entry_tree(&directory, &entry)?;
    }
    parent.rmdir(name)
}

impl RunStore {
    /// Verify the original artifact before returning bounded raw evidence.
    /// The returned digest identifies the original artifact.
    pub fn stream_result(
        &self,
        id: provenant_core::AttemptId,
        call: &Request<'_>,
        required_verdict: Option<&str>,
    ) -> Result<provenant_records::result_capture::CapturedResult> {
        self.stream_result_observing(id, call, required_verdict, || {})
    }
    /// Like `stream_result`, with `observing` invoked between the streamed EOF
    /// and the final metadata check, so tests can grow the file through a
    /// second handle in exactly the window the refusal must name.
    pub(crate) fn stream_result_observing(
        &self,
        id: provenant_core::AttemptId,
        call: &Request<'_>,
        required_verdict: Option<&str>,
        mut observing: impl FnMut(),
    ) -> Result<provenant_records::result_capture::CapturedResult> {
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
        let dir = self.attempt_dir(id, false)?;
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
            capture.push(&buffer[..count]);
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
        observing();
        let after = file.metadata()?;
        if !before.unchanged(&after) {
            let grew = before.size != after.size;
            return Err(RunError::artifact_mismatch(
                ArtifactCheck::Stream,
                "result.md",
                grew.then_some(ArtifactValue::Length(artifact.bytes)),
                grew.then_some(ArtifactValue::Length(after.size)),
            ));
        }
        let mut result = capture.finish();
        // One handoff rule: the settled attempt's sealed snapshot,
        // not a re-reading of the text. The handoff is auxiliary to the result:
        // a slot read error degrades to the malformed-handoff marker so a
        // defect in the snapshot never hides result.md (#3844).
        result.handoff = self
            .handoff(&record)
            .unwrap_or_else(|_| Err(HandoffError::new("$", "handoff snapshot unreadable")));
        result.digest = digest.to_string();
        Ok(result)
    }

    /// An attempt's directory: `attempts/<AttemptId>` under the store root.
    /// Receipt paths are relative to the root, and node ids never appear.
    pub(crate) fn attempt_dir(
        &self,
        attempt: provenant_core::AttemptId,
        create: bool,
    ) -> Result<Directory> {
        let attempts = subdir(&self.root, OsStr::new("attempts"), create)?;
        subdir(&attempts, OsStr::new(&attempt.to_string()), create)
    }
    pub(crate) fn artifact_bytes(&self, artifact: &ArtifactRecord, limit: u64) -> Result<Vec<u8>> {
        if artifact.bytes > limit {
            return Err(RunError::TooLarge { limit });
        }
        // Receipt paths are relative to the store root.
        let mut dir: Option<Directory> = None;
        let path = artifact.path.to_relative_path()?;
        let mut parts = path.as_path().components().peekable();
        while let Some(Component::Normal(name)) = parts.next() {
            if parts.peek().is_some() {
                dir = Some(subdir(dir.as_ref().unwrap_or(&self.root), name, false)?);
            } else {
                let dir = dir.as_ref().unwrap_or(&self.root);
                if let Some(check) = PathCheck::file(dir.metadata_at(name, false)?) {
                    return Err(RunError::unsafe_entry(dir, name, check));
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
#[cfg(test)]
thread_local! {
    /// Rendered private-mode repair warnings, for tests that cannot read the
    /// process-wide stderr stream without racing other tests. Thread-local so
    /// each test collects only its own repairs and no `static` needs `Sync` for
    /// the non-`Sync` `RefCell` it holds; the production warning still goes to
    /// stderr.
    pub(crate) static PRIVATE_MODE_REPAIRS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// What a private read may do about an over-broad mode. Every sealed read
/// refuses it, because a mode defect outside a provider-written settlement
/// input reports something the owner must see. [`read_settled`] repairs only
/// entries in that class.
#[derive(Clone, Copy)]
enum ModePolicy {
    Refuse,
    Repair,
}

/// Judge a sealed regular file through its open descriptor. A too-broad mode
/// is repaired in place with a warning naming the path and the old mode only
/// when `repair` is set, which [`read_settled`] alone does for a
/// provider-written settlement input; a link, foreign owner, symlink or other
/// kind stays refused. `repair` is false for a handle already retired by a
/// rename, which the caller retries instead.
fn private_metadata(
    dir: &Directory,
    name: &OsStr,
    file: &FileHandle,
    meta: Metadata,
    repair: bool,
) -> Result<Metadata> {
    if let Some(check) = PathCheck::file(meta) {
        if check != PathCheck::Mode || !repair {
            return Err(RunError::unsafe_entry(dir, name, check));
        }
        let old_mode = file.make_owner_private()?;
        let after = file.metadata()?;
        if let Some(check) = PathCheck::file(after) {
            return Err(RunError::unsafe_entry(dir, name, check));
        }
        if let Some(old_mode) = old_mode {
            let rendered = format!(
                "warning: tightened run-store path {} from mode {:04o} to a private mode",
                dir.anchored_path().join(name).display(),
                old_mode & 0o7777
            );
            #[cfg(test)]
            PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow_mut().push(rendered.clone()));
            use provenant_records::warning::{RUN_STORE_MODE_TIGHTENED, Warning, warn};
            // Spec: docs/specs/cli.md#warnings-are-typed-values
            warn(
                Warning::new(RUN_STORE_MODE_TIGHTENED, rendered.clone())
                    .subject(dir.anchored_path().join(name).display().to_string()),
            );
        }
        return Ok(after);
    }
    Ok(meta)
}

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
/// The settlement read of a provider-written settlement input. Settlement
/// tightens an over-broad mode on these files through the open descriptor,
/// warns with the path and the old mode, and continues rather than stranding a
/// finished writer. The class lives on [`AttemptFile`], so a new
/// provider-written input is covered by declaring it there; a host-written
/// record still refuses, and every other read of the same name does too, so a
/// store defect stays visible.
pub(crate) fn read_settled(
    dir: &Directory,
    file: AttemptFile,
    limit: u64,
) -> Result<Option<Vec<u8>>> {
    let policy = if file.provider_settlement_input() {
        ModePolicy::Repair
    } else {
        ModePolicy::Refuse
    };
    read_sealed_with(dir, file.name(), limit, true, policy, || {})
}
/// A read-only observation of a sealed name. It skips the two syncs that make
/// [`read_sealed`] durable, so callers report what is published without paying
/// an `F_FULLFSYNC` per read. Never use the bytes to decide a write.
pub(crate) fn read_sealed_observed(
    dir: &Directory,
    name: &str,
    limit: u64,
) -> Result<Option<Vec<u8>>> {
    read_sealed_with(dir, name, limit, false, ModePolicy::Refuse, || {})
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
    read_sealed_with(dir, name, limit, true, ModePolicy::Refuse, opened)
}
fn read_sealed_with(
    dir: &Directory,
    name: &str,
    limit: u64,
    durable: bool,
    policy: ModePolicy,
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
        let repair = matches!(policy, ModePolicy::Repair) && !unlinked;
        private_metadata(dir, OsStr::new(name), &file, judged, repair)?;
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
/// Publish an advisory notice that a later settlement may supersede. Unlike
/// [`seal`] it never conflicts with an earlier, different notice.
pub(crate) fn replace(dir: &Directory, name: &str, content: &[u8], limit: u64) -> Result<()> {
    if content.len() as u64 > limit {
        return Err(RunError::TooLarge { limit });
    }
    dir.atomic_write(OsStr::new(name), ".run-", content, PublishMode::Replace)?
        .require_durable()
        .map_err(RunError::Publication)
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
    fn terminal_record_bound_reads_old_and_larger_decisions() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        seal(&dir, "old.json", b"old", TERMINAL_RECORD_LIMIT)?;
        assert_eq!(
            read_sealed(&dir, "old.json", TERMINAL_RECORD_LIMIT)?.as_deref(),
            Some(b"old".as_slice())
        );
        let larger = vec![b'x'; RECORD_LIMIT as usize + 1];
        seal(
            &dir,
            provenant_records::AttemptFile::TerminalPending.name(),
            &larger,
            TERMINAL_RECORD_LIMIT,
        )?;
        assert_eq!(
            read_sealed(
                &dir,
                provenant_records::AttemptFile::TerminalPending.name(),
                TERMINAL_RECORD_LIMIT
            )?
            .as_deref(),
            Some(larger.as_slice())
        );
        assert!(
            read_sealed(
                &dir,
                provenant_records::AttemptFile::TerminalPending.name(),
                RECORD_LIMIT
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn checkpoint_reads_follow_replacement_for_each_durability_mode() -> TestResult {
        for durable in [false, true] {
            let temp = tempfile::tempdir()?;
            let dir = Directory::open_anchor(temp.path())?;
            checkpoint(&dir, b"old")?;
            let mut replaced = false;
            let observe = || {
                if !replaced {
                    // Replace the checkpoint between open and the final name check.
                    checkpoint(&dir, b"new").expect("replace checkpoint");
                    replaced = true;
                }
            };
            let read = if durable {
                read_sealed_observing(&dir, "process.json", 1024, observe)?
            } else {
                read_sealed_with(
                    &dir,
                    "process.json",
                    1024,
                    false,
                    ModePolicy::Refuse,
                    observe,
                )?
            };
            assert_eq!(read.as_deref(), Some(&b"new"[..]));
            assert_eq!(read, read_sealed(&dir, "process.json", 1024)?);
        }
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
    fn read_sealed_refuses_a_broad_mode_and_never_tightens_it() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        checkpoint(&dir, b"x")?;
        let name = OsStr::new("process.json");
        let meta = dir.metadata_at(name, false)?;
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
        PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow_mut().clear());
        std::fs::set_permissions(
            temp.path().join("process.json"),
            std::fs::Permissions::from_mode(0o644),
        )?;
        let error = read_sealed(&dir, "process.json", 1024).expect_err("group-readable");
        assert!(matches!(
            error,
            RunError::UnsafePath {
                path,
                check: PathCheck::Mode,
            } if path == dir.anchored_path().join("process.json")
        ));
        assert_eq!(
            dir.metadata_at(name, false)?.mode & 0o077,
            0o044,
            "a private read must not repair the mode"
        );
        assert!(
            PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow().is_empty()),
            "a refused read must not report a repair"
        );
        Ok(())
    }

    #[test]
    fn read_settled_repairs_a_broad_mode_on_every_provider_settlement_input() -> TestResult {
        for file in AttemptFile::PROVIDER_SETTLEMENT_INPUTS {
            let temp = tempfile::tempdir()?;
            let dir = Directory::open_anchor(temp.path())?;
            seal(&dir, file.name(), b"x", 1024)?;
            PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow_mut().clear());
            let path = temp.path().join(file.name());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
            assert_eq!(
                read_settled(&dir, file, 1024)?.as_deref(),
                Some(b"x".as_slice()),
                "{}",
                file.name()
            );
            let warnings = PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow().clone());
            assert_eq!(warnings.len(), 1, "{}: {warnings:?}", file.name());
            assert!(
                warnings[0].contains(&dir.anchored_path().join(file.name()).display().to_string()),
                "{}: warning must name the repaired path: {warnings:?}",
                file.name()
            );
            assert!(
                warnings[0].contains("0644"),
                "{}: warning must name the old mode: {warnings:?}",
                file.name()
            );
            let after = dir.metadata_at(OsStr::new(file.name()), false)?;
            assert_eq!(after.mode & 0o077, 0, "{}", file.name());
            assert_eq!(PathCheck::file(after), None, "{}", file.name());
        }
        Ok(())
    }

    #[test]
    fn read_settled_refuses_a_broad_mode_on_a_host_written_record() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        checkpoint(&dir, b"x")?;
        PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow_mut().clear());
        let name = OsStr::new(AttemptFile::Process.name());
        std::fs::set_permissions(
            temp.path().join(AttemptFile::Process.name()),
            std::fs::Permissions::from_mode(0o644),
        )?;
        let error = read_settled(&dir, AttemptFile::Process, 1024).expect_err("group-readable");
        assert!(matches!(
            error,
            RunError::UnsafePath {
                check: PathCheck::Mode,
                ..
            }
        ));
        assert_eq!(
            dir.metadata_at(name, false)?.mode & 0o077,
            0o044,
            "a host-written record must never be repaired"
        );
        assert!(
            PRIVATE_MODE_REPAIRS.with(|warnings| warnings.borrow().is_empty()),
            "a refused read must not report a repair"
        );
        Ok(())
    }

    #[test]
    fn read_settled_refuses_a_symlink_instead_of_tightening_its_target() -> TestResult {
        let temp = tempfile::tempdir()?;
        let dir = Directory::open_anchor(temp.path())?;
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, b"sentinel")?;
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o644))?;
        std::os::unix::fs::symlink(&outside, temp.path().join("result.md"))?;
        let error = read_settled(&dir, AttemptFile::Result, 1024).expect_err("symlinked result");
        assert!(
            matches!(error, RunError::Filesystem(_) | RunError::UnsafePath { .. }),
            "a link must never be read or repaired: {error:?}"
        );
        assert_eq!(
            std::fs::metadata(&outside)?.permissions().mode() & 0o077,
            0o044,
            "the outside target must keep its mode"
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
        let id = run.attempt_id;
        store.create(&run, &call)?;
        seal(&store.attempt_dir(id, false)?, "result.md", b"hello", 1024)?;
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
        assert_eq!(store.artifact_bytes(&honest, 1024)?, b"hello");
        let artifact = format!("attempts/{id}/result.md");
        assert_eq!(
            crate::tests::artifact_provenance(
                store.artifact_bytes(&record(6, CanonicalV1::file_digest(b"hello")), 1024)
            ),
            ArtifactProvenance {
                check: ArtifactCheck::RecordedLength,
                artifact: artifact.clone(),
                expected: Some(ArtifactValue::Length(6)),
                actual: Some(ArtifactValue::Length(5)),
            }
        );
        assert_eq!(
            crate::tests::artifact_provenance(
                store.artifact_bytes(&record(5, CanonicalV1::file_digest(b"hellp")), 1024)
            ),
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
        let attempt = run.attempt_id;
        let relative = Path::new("attempts").join(attempt.to_string());
        std::fs::set_permissions(
            context.state_root.as_path().join(&relative),
            std::fs::Permissions::from_mode(0o755),
        )?;
        let error = store
            .attempt_dir(attempt, false)
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

    #[test]
    fn stream_result_with_long_withheld_run_yields_verdict() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let attempt = run.attempt_id;
        crate::tests::seal_exit_fixture(&store, &run)?;
        let slashes = "\\".repeat(1024 * 1024 - 1);
        let text = format!("password={}\"secret\" end\n\nVERDICT: PASS\n", slashes);
        let mut done = crate::tests::publication();
        done.result = &text;
        let exit = crate::tests::exit();
        store.publish_terminal(attempt, 1, &done, &exit, &call)?;
        let result = store.stream_result(attempt, &call, None)?;
        assert_eq!(result.verdict.as_deref(), Some("VERDICT: PASS"));
        Ok(())
    }

    /// #3844: the settled handoff is auxiliary to the result. An unreadable
    /// slot must not hide `result.md`: capture degrades to the malformed-handoff
    /// marker and the answer still streams.
    // Spec: docs/specs/run-settlement.md#outcome-before-release
    #[test]
    fn stream_result_keeps_the_answer_when_the_settled_slot_is_unreadable() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = crate::tests::clock();
        let cancel = provenant_core::CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let context = crate::tests::context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = crate::tests::input();
        store.create(&run, &call)?;
        let attempt = run.attempt_id;
        crate::tests::seal_exit_fixture(&store, &run)?;
        let text = "password=\"secret\" end\n\nVERDICT: PASS\n";
        let mut done = crate::tests::publication();
        done.result = &text;
        let exit = crate::tests::exit();
        store.publish_terminal(attempt, 1, &done, &exit, &call)?;

        // A link in the slot fails the typed sealed read; stream_result must
        // keep the captured answer and mark the handoff malformed.
        let dir = store.attempt_dir(attempt, false)?;
        let slot = dir.current_path()?.join(AttemptFile::Handoff.name());
        if slot.symlink_metadata().is_ok() {
            std::fs::remove_file(&slot)?;
        }
        std::os::unix::fs::symlink("not-read-target", &slot)?;

        let result = store.stream_result(attempt, &call, None)?;
        assert_eq!(result.verdict.as_deref(), Some("VERDICT: PASS"));
        assert!(
            matches!(&result.handoff, Err(error) if error.to_string().contains("handoff snapshot unreadable")),
            "an unreadable slot degrades to a malformed handoff: {:?}",
            result.handoff
        );
        assert!(
            result.excerpt.contains("password="),
            "the captured answer still streams: {:?}",
            result.excerpt
        );
        Ok(())
    }
}
