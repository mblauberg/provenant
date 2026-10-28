//! Bounded retention for host-owned Cargo target generations.
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
}

/// Collect oldest generations until both limits hold. A busy store is left untouched.
pub fn collect(
    state: &Path,
    max_generations: usize,
    max_bytes: u64,
    dry_run: bool,
) -> Result<Report, Box<dyn std::error::Error>> {
    let _lock = crate::target_seed::lock_store(state)?;
    let store = crate::target_seed::store_path(state);
    let mut report = Report::default();
    // A single build lock protects all stages. After a crash the OS releases
    // it, allowing the next collection to reclaim every abandoned tree.
    if let Ok(_build_lock) = crate::target_seed::lock_build(state) {
        for entry in fs::read_dir(&store)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with(".staging-")
                && entry.file_type()?.is_dir()
            {
                remove(&entry.path(), dry_run, &mut report)?;
            }
        }
    }
    let root = store.join("generations");
    let mut generations = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if fs::read(entry.path().join("complete")).ok().as_deref() != Some(b"complete\n")
                || entry.metadata()?.permissions().mode() & 0o222 != 0
            {
                remove(&entry.path(), dry_run, &mut report)?;
                continue;
            }
            let size = apparent_size(&entry.path())?;
            generations.push((entry.file_name(), entry.path(), size));
        }
    }
    generations.sort_by(|a, b| a.0.cmp(&b.0));
    let mut total = generations
        .iter()
        .fold(0u64, |sum, item| sum.saturating_add(item.2));
    let mut kept = generations.len();
    for (_, path, size) in generations {
        if kept > max_generations || total > max_bytes {
            if !dry_run {
                make_directories_writable(&path)?;
                fs::remove_dir_all(&path)?;
            }
            report.bytes = report.bytes.saturating_add(size);
            report.removed.push(path);
            kept -= 1;
            total = total.saturating_sub(size);
        }
    }
    Ok(report)
}

fn remove(path: &Path, dry_run: bool, report: &mut Report) -> io::Result<()> {
    let size = apparent_size(path)?;
    if !dry_run {
        make_directories_writable(path)?;
        fs::remove_dir_all(path)?;
    }
    report.bytes = report.bytes.saturating_add(size);
    report.removed.push(path.to_path_buf());
    Ok(())
}

fn apparent_size(path: &Path) -> io::Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut size = 0u64;
    for entry in fs::read_dir(path)? {
        size = size.saturating_add(apparent_size(&entry?.path())?);
    }
    Ok(size)
}

// Published trees can have read-only directory permissions. Never follow links.
fn make_directories_writable(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(path, permissions)?;
    }
    for entry in fs::read_dir(path)? {
        make_directories_writable(&entry?.path())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn generation(state: &Path, id: &str, bytes: usize) -> PathBuf {
        let dir = crate::target_seed::store_path(state)
            .join("generations")
            .join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("payload"), vec![0; bytes]).unwrap();
        fs::write(dir.join("complete"), b"complete\n").unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        dir
    }
    #[test]
    fn retention_obeys_count_and_bytes_and_preview() {
        let state = tempfile::tempdir().unwrap();
        let old = generation(state.path(), "001", 3);
        let middle = generation(state.path(), "002", 4);
        let newest = generation(state.path(), "003", 5);
        let preview = collect(state.path(), 2, 20, true).unwrap();
        assert_eq!(preview.removed, vec![old.clone(), middle.clone()]);
        assert!(middle.exists());
        let result = collect(state.path(), 1, 100, false).unwrap();
        assert_eq!(result.removed, vec![old.clone(), middle.clone()]);
        assert!(newest.exists());
        assert!(!middle.exists() && !old.exists());
    }
    #[test]
    fn active_clone_lock_prevents_collection() {
        let state = tempfile::tempdir().unwrap();
        let generation = generation(state.path(), "001", 3);
        let guard = crate::target_seed::lock_store(state.path()).unwrap();
        assert!(collect(state.path(), 0, 0, false).is_err());
        assert!(generation.exists());
        drop(guard);
        collect(state.path(), 0, 0, false).unwrap();
        assert!(!generation.exists());
    }
    #[test]
    fn abandoned_stages_and_incomplete_generations_are_reclaimed() {
        let state = tempfile::tempdir().unwrap();
        let guard = crate::target_seed::lock_store(state.path()).unwrap();
        let build = crate::target_seed::lock_build(state.path()).unwrap();
        let stage = crate::target_seed::store_path(state.path()).join(".staging-test");
        fs::create_dir(&stage).unwrap();
        let incomplete = generation(state.path(), "001", 3);
        make_directories_writable(&incomplete).unwrap();
        let missing_marker = generation(state.path(), "002", 3);
        make_directories_writable(&missing_marker).unwrap();
        fs::remove_file(missing_marker.join("complete")).unwrap();
        drop(guard);
        collect(state.path(), 10, 100, false).unwrap();
        assert!(stage.exists());
        assert!(!incomplete.exists());
        assert!(!missing_marker.exists());
        drop(build);
        collect(state.path(), 10, 100, false).unwrap();
        assert!(!stage.exists());
    }
}
