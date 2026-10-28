//! Bounded retention for host-owned Cargo target generations.
use std::{
    fs, io,
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
    let root = crate::target_seed::store_path(state).join("generations");
    let mut generations = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let size = apparent_size(&entry.path())?;
            generations.push((entry.file_name(), entry.path(), size));
        }
    }
    generations.sort_by(|a, b| a.0.cmp(&b.0));
    let mut total = generations
        .iter()
        .fold(0u64, |sum, item| sum.saturating_add(item.2));
    let mut kept = generations.len();
    let mut report = Report::default();
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
        dir
    }
    #[test]
    fn retention_obeys_count_and_bytes_and_preview() {
        let state = tempfile::tempdir().unwrap();
        let old = generation(state.path(), "001", 3);
        let middle = generation(state.path(), "002", 4);
        let newest = generation(state.path(), "003", 5);
        let preview = collect(state.path(), 2, 8, true).unwrap();
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
}
