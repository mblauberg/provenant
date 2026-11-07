//! Reclaim abandoned state-root publication scratch under its owner locks.
use provenant_os::fs::{Directory, FileKind, FsError, LockMode, PayloadTree};
use std::{ffi::OsStr, path::Path};

fn clear(tree: &PayloadTree, depth: usize) -> Result<(), FsError> {
    if depth > 64 {
        return Err(FsError::EntryLimit { limit: 64 });
    }
    tree.make_writable()?;
    for name in tree.entries()? {
        let name = name?;
        if tree.metadata_at(&name)?.kind == FileKind::Directory {
            let child = tree.child(&name)?;
            clear(&child, depth + 1)?;
            tree.rmdir(&name)?;
        } else {
            tree.unlink(&name)?;
        }
    }
    Ok(())
}

pub(crate) fn collect(state: &Path, before: i128, dry_run: bool) -> Result<(), FsError> {
    let root = Directory::open_anchor(state)?;
    let ingest = root
        .try_lock_existing(OsStr::new("ingest.lock"), LockMode::Exclusive)
        .ok();
    let install = root.open_directory(OsStr::new("install")).ok();
    let install_lock = install.as_ref().and_then(|dir| {
        dir.try_lock_existing(OsStr::new("install.lock"), LockMode::Exclusive)
            .ok()
    });
    for name in root.entries()? {
        let name = name?;
        let Some(text) = name.to_str() else { continue };
        let metadata = root.metadata_at(&name, false)?;
        if metadata.modified_ns >= before {
            continue;
        }
        if text.starts_with(".ingest-") && metadata.kind == FileKind::Regular && ingest.is_some() {
            if !dry_run {
                root.unlink(&name)?;
            }
        } else if text.starts_with(".provenant-staging.")
            && metadata.kind == FileKind::Directory
            && install_lock.is_some()
        {
            let tree = PayloadTree::claim(&root, &name, &metadata)?;
            if !dry_run {
                clear(&tree, 0)?;
                root.rmdir(&name)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_scratch_is_removed_only_with_owner_locks() {
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
        collect(temp.path(), i128::MAX, true).unwrap();
        assert!(ingest.exists() && stage.exists());
        collect(temp.path(), i128::MAX, false).unwrap();
        assert!(!ingest.exists() && stage.exists());
        drop(lock);
        collect(temp.path(), 0, false).unwrap();
        assert!(stage.exists());
        collect(temp.path(), i128::MAX, false).unwrap();
        assert!(!stage.exists());
    }
}
