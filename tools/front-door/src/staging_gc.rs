//! Reclaim abandoned state-root publication scratch under its owner locks.
use provenant_core::CancelToken;
use provenant_os::fs::{
    Directory, DirectoryPosition, Entries, FileKind, FsError, LockMode, PayloadTree,
};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

#[derive(Debug, Default)]
pub(crate) struct Report {
    pub(crate) busy: Vec<PathBuf>,
    pub(crate) interrupted_at: Option<PathBuf>,
    pub(crate) continuation: Option<String>,
}

fn root_cursor(position: DirectoryPosition) -> String {
    format!("staging-root-v1:{}:{}", position.block, position.offset)
}

fn root_cursor_position(detail: Option<&str>) -> Option<DirectoryPosition> {
    let mut components = detail?.strip_prefix("staging-root-v1:")?.split(':');
    let position = DirectoryPosition {
        block: components.next()?.parse().ok()?,
        offset: components.next()?.parse().ok()?,
    };
    components.next().is_none().then_some(position)
}

fn clear(
    tree: &PayloadTree,
    depth: usize,
    cancel: &CancelToken,
    should_stop: &mut dyn FnMut() -> bool,
) -> Result<bool, FsError> {
    if depth > 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    tree.make_writable()?;
    for name in tree.entries()? {
        if cancel.is_cancelled() || should_stop() {
            return Ok(false);
        }
        let name = name?;
        if tree.metadata_at(&name)?.kind == FileKind::Directory {
            let child = tree.child(&name)?;
            if !clear(&child, depth + 1, cancel, should_stop)? {
                return Ok(false);
            }
            tree.rmdir(&name)?;
        } else {
            tree.unlink(&name)?;
        }
    }
    Ok(true)
}

pub(crate) fn collect(
    state: &Path,
    before: i128,
    dry_run: bool,
    cancel: &CancelToken,
    start_at: Option<&Path>,
    detail: Option<&str>,
    should_stop: &mut dyn FnMut() -> bool,
) -> Result<Report, FsError> {
    let root = Directory::open_anchor(state)?;
    let ingest = root
        .try_lock_existing(OsStr::new("ingest.lock"), LockMode::Exclusive)
        .ok();
    let install = root.open_directory(OsStr::new("install")).ok();
    let install_lock = install.as_ref().and_then(|dir| {
        dir.try_lock_existing(OsStr::new("install.lock"), LockMode::Exclusive)
            .ok()
    });
    collect_entries(
        &root,
        root.entries()?,
        before,
        dry_run,
        ingest.is_some(),
        install_lock.is_some(),
        cancel,
        start_at,
        detail,
        should_stop,
    )
}

fn collect_entries(
    root: &Directory,
    entries: Entries,
    before: i128,
    dry_run: bool,
    ingest_locked: bool,
    install_locked: bool,
    cancel: &CancelToken,
    start_at: Option<&Path>,
    detail: Option<&str>,
    should_stop: &mut dyn FnMut() -> bool,
) -> Result<Report, FsError> {
    let mut report = Report::default();
    let mut entries = entries;
    let mut pending = None;
    if let (Some(start), Some(position)) = (start_at, root_cursor_position(detail)) {
        if entries.seek(position).is_ok() {
            let entry_offset = entries.position()?;
            if let Some(entry) = entries.next() {
                let name = entry?;
                if root.current_path()?.join(&name) == start {
                    pending = Some((name, entry_offset));
                } else {
                    entries = root.entries()?;
                }
            } else {
                entries = root.entries()?;
            }
        }
    }
    loop {
        let (name, entry_offset) = match pending.take() {
            Some(pending) => pending,
            None => {
                let entry_offset = entries.position()?;
                let Some(name) = entries.next() else {
                    break;
                };
                (name?, entry_offset)
            }
        };
        let path = root.current_path()?.join(&name);
        if cancel.is_cancelled() || should_stop() {
            report.interrupted_at = Some(path);
            report.continuation = Some(root_cursor(entry_offset));
            break;
        }
        let Some(text) = name.to_str() else { continue };
        let is_ingest = text.starts_with(".ingest-") && ingest_locked;
        let is_staging = text.starts_with(".provenant-staging.") && install_locked;
        if !is_ingest && !is_staging {
            if text.starts_with(".ingest-") || text.starts_with(".provenant-staging.") {
                report.busy.push(path);
            }
            continue;
        }
        let metadata = match root.metadata_at(&name, false) {
            Ok(metadata) => metadata,
            Err(error) if error.is_not_found() => continue,
            Err(error) => return Err(error),
        };
        if metadata.modified_ns >= before && start_at != Some(path.as_path()) {
            continue;
        }
        if is_ingest && metadata.kind == FileKind::Regular {
            if !dry_run {
                if cancel.is_cancelled() || should_stop() {
                    report.interrupted_at = Some(path);
                    report.continuation = Some(root_cursor(entry_offset));
                    break;
                }
                root.unlink(&name)?;
            }
        } else if is_staging && metadata.kind == FileKind::Directory {
            let tree = PayloadTree::claim(root, &name, &metadata)?;
            if !dry_run {
                if !clear(&tree, 0, cancel, should_stop)? {
                    report.interrupted_at = Some(path);
                    report.continuation = Some(root_cursor(entry_offset));
                    break;
                }
                if cancel.is_cancelled() || should_stop() {
                    report.interrupted_at = Some(path);
                    report.continuation = Some(root_cursor(entry_offset));
                    break;
                }
                root.rmdir(&name)?;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    #[test]
    fn filters_before_metadata_and_skips_vanished_entries() {
        use super::*;
        let temp = tempfile::tempdir().unwrap();
        let root = Directory::open_anchor(temp.path()).unwrap();
        std::fs::write(temp.path().join(".ingest-survivor"), b"data").unwrap();
        std::fs::write(temp.path().join("unrelated"), b"data").unwrap();
        collect_entries(
            &root,
            root.entries().unwrap(),
            i128::MAX,
            false,
            true,
            false,
            &CancelToken::new(),
            None,
            None,
            &mut || false,
        )
        .unwrap();
        assert!(!temp.path().join(".ingest-survivor").exists());

        collect_entries(
            &root,
            root.entries().unwrap(),
            i128::MAX,
            false,
            false,
            false,
            &CancelToken::new(),
            None,
            None,
            &mut || false,
        )
        .unwrap();
    }

    #[test]
    fn old_scratch_is_removed_only_with_owner_locks() {
        use super::*;
        let temp = tempfile::tempdir().unwrap();
        let root = Directory::open_anchor(temp.path()).unwrap();
        std::fs::create_dir(temp.path().join("install")).unwrap();
        let install = root.open_directory(OsStr::new("install")).unwrap();
        drop(
            root.try_lock(OsStr::new("ingest.lock"), LockMode::Exclusive)
                .unwrap(),
        );
        let lock = install
            .try_lock(OsStr::new("install.lock"), LockMode::Exclusive)
            .unwrap();
        let stage = temp.path().join(".provenant-staging.old");
        std::fs::create_dir(&stage).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(stage.join("file"), b"data").unwrap();
        let ingest = temp.path().join(".ingest-old");
        std::fs::write(&ingest, b"data").unwrap();
        collect(
            temp.path(),
            i128::MAX,
            true,
            &CancelToken::new(),
            None,
            None,
            &mut || false,
        )
        .unwrap();
        assert!(ingest.exists() && stage.exists());
        let busy = collect(
            temp.path(),
            i128::MAX,
            false,
            &CancelToken::new(),
            None,
            None,
            &mut || false,
        )
        .unwrap();
        assert_eq!(busy.busy, vec![PathBuf::from(".provenant-staging.old")]);
        assert!(!ingest.exists() && stage.exists());
        drop(lock);
        collect(
            temp.path(),
            0,
            false,
            &CancelToken::new(),
            None,
            None,
            &mut || false,
        )
        .unwrap();
        assert!(stage.exists());
        collect(
            temp.path(),
            i128::MAX,
            false,
            &CancelToken::new(),
            None,
            None,
            &mut || false,
        )
        .unwrap();
        assert!(!stage.exists());
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn interrupted_staging_tree_resumes_from_its_candidate() {
        use super::*;
        let temp = tempfile::tempdir().unwrap();
        let root = Directory::open_anchor(temp.path()).unwrap();
        let install_path = temp.path().join("install");
        std::fs::create_dir(&install_path).unwrap();
        let install = root.open_directory(OsStr::new("install")).unwrap();
        let _ingest = root
            .try_lock(OsStr::new("ingest.lock"), LockMode::Exclusive)
            .unwrap();
        let _install = install
            .try_lock(OsStr::new("install.lock"), LockMode::Exclusive)
            .unwrap();
        let stage = temp.path().join(".provenant-staging.large");
        std::fs::create_dir(&stage).unwrap();
        for index in 0..128 {
            std::fs::write(stage.join(format!("payload-{index:03}")), b"data").unwrap();
        }
        let mut checked = 0;
        let partial = collect(
            temp.path(),
            i128::MAX,
            false,
            &CancelToken::new(),
            None,
            None,
            &mut || {
                checked += 1;
                checked <= 12
            },
        )
        .unwrap();
        assert_eq!(partial.interrupted_at.as_deref(), Some(stage.as_path()));
        let remaining = std::fs::read_dir(&stage).unwrap().count();
        assert!(remaining > 0 && remaining < 128);

        let resumed = collect(
            temp.path(),
            i128::MAX,
            false,
            &CancelToken::new(),
            partial.interrupted_at.as_deref(),
            partial.continuation.as_deref(),
            &mut || true,
        )
        .unwrap();
        assert!(resumed.interrupted_at.is_none());
        assert!(!stage.exists());
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn interrupted_staging_root_listing_resumes_from_its_directory_cookie() {
        use super::*;
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("ingest.lock"), b"").unwrap();
        for index in 0..64 {
            std::fs::write(temp.path().join(format!(".ingest-{index:03}")), b"data").unwrap();
        }

        let mut checked = 0;
        let partial = collect(
            temp.path(),
            i128::MAX,
            false,
            &CancelToken::new(),
            None,
            None,
            &mut || {
                checked += 1;
                checked <= 12
            },
        )
        .unwrap();
        let candidate = partial
            .interrupted_at
            .clone()
            .expect("root scan interrupted");
        let detail = partial
            .continuation
            .clone()
            .expect("root scan saved its entry cookie");
        let remaining = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".ingest-"))
            .count();
        assert!(remaining > 0 && remaining < 64);

        let resumed = collect(
            temp.path(),
            i128::MAX,
            false,
            &CancelToken::new(),
            Some(&candidate),
            Some(&detail),
            &mut || true,
        )
        .unwrap();
        assert!(resumed.interrupted_at.is_none());
        assert_eq!(
            std::fs::read_dir(temp.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(".ingest-"))
                .count(),
            0
        );
    }
}
