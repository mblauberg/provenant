//! Bounded retention for host-owned Cargo target generations.
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
    pub busy: Vec<PathBuf>,
}

/// Collect oldest generations until both limits hold. A busy store is left untouched.
pub fn collect(
    state: &Path,
    max_generations: usize,
    max_bytes: u64,
    dry_run: bool,
) -> Result<Report, Box<dyn std::error::Error>> {
    let store = crate::target_seed::store_path(state);
    let mut report = Report::default();
    let _lock = match crate::target_seed::lock_store(state) {
        Ok(lock) => lock,
        Err(error)
            if matches!(
                error.downcast_ref::<provenant_os::fs::FsError>(),
                Some(provenant_os::fs::FsError::LockTimeout { .. })
            ) =>
        {
            report.busy.push(store);
            return Ok(report);
        }
        Err(error) => return Err(error),
    };
    // A single build lock protects all stages. After a crash the OS releases
    // it, allowing the next collection to reclaim every abandoned tree.
    match crate::target_seed::lock_build(state) {
        Ok(_build_lock) => {
            for entry in fs::read_dir(&store)? {
                let entry = entry?;
                if entry.file_name().to_string_lossy().starts_with(".staging-")
                    && entry.file_type()?.is_dir()
                {
                    remove(&entry.path(), dry_run, &mut report)?;
                }
            }
        }
        Err(error)
            if matches!(
                error.downcast_ref::<provenant_os::fs::FsError>(),
                Some(provenant_os::fs::FsError::LockTimeout { .. })
            ) =>
        {
            report.busy.push(store.clone())
        }
        Err(error) => return Err(error),
    }
    let root = store.join("generations");
    struct Generation {
        id: std::ffi::OsString,
        path: PathBuf,
        size: u64,
        project_key: String,
    }
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
            let size = allocated_size(&entry.path())?;
            let project_key = fs::read(entry.path().join("manifest.json"))
                .ok()
                .and_then(|bytes| {
                    serde_json::from_slice::<crate::target_seed::Manifest>(&bytes).ok()
                })
                .map_or_else(String::new, |manifest| manifest.project_key);
            generations.push(Generation {
                id: entry.file_name(),
                path: entry.path(),
                size,
                project_key,
            });
        }
    }
    generations.sort_by(|a, b| a.id.cmp(&b.id));
    let mut total = generations
        .iter()
        .fold(0u64, |sum, item| sum.saturating_add(item.size));
    let mut per_project = BTreeMap::<String, usize>::new();
    let mut newest_by_project = BTreeMap::<String, std::ffi::OsString>::new();
    for generation in &generations {
        *per_project
            .entry(generation.project_key.clone())
            .or_default() += 1;
        // IDs are monotonic; generations are sorted by ID, so the last ID for
        // each project is its newest complete seed and remains warm.
        newest_by_project.insert(generation.project_key.clone(), generation.id.clone());
    }
    for generation in generations {
        let Some(kept) = per_project.get_mut(&generation.project_key) else {
            return Err(io::Error::other("target seed project count changed").into());
        };
        if newest_by_project.get(&generation.project_key) == Some(&generation.id) {
            continue;
        }
        if *kept > max_generations || total > max_bytes {
            if !dry_run {
                make_directories_writable(&generation.path)?;
                fs::remove_dir_all(&generation.path)?;
            }
            report.bytes = report.bytes.saturating_add(generation.size);
            report.removed.push(generation.path);
            *kept -= 1;
            total = total.saturating_sub(generation.size);
        }
    }
    Ok(report)
}

fn remove(path: &Path, dry_run: bool, report: &mut Report) -> io::Result<()> {
    let size = allocated_size(path)?;
    if !dry_run {
        make_directories_writable(path)?;
        fs::remove_dir_all(path)?;
    }
    report.bytes = report.bytes.saturating_add(size);
    report.removed.push(path.to_path_buf());
    Ok(())
}

fn allocated_size(path: &Path) -> io::Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    use std::os::unix::fs::MetadataExt;
    let mut size = metadata.blocks().saturating_mul(512);
    if !metadata.is_dir() {
        return Ok(size);
    }
    for entry in fs::read_dir(path)? {
        size = size.saturating_add(allocated_size(&entry?.path())?);
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
        generation_for_project(state, id, bytes, "project")
    }

    fn generation_for_project(state: &Path, id: &str, bytes: usize, project_key: &str) -> PathBuf {
        let dir = crate::target_seed::store_path(state)
            .join("generations")
            .join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("payload"), vec![0; bytes]).unwrap();
        fs::write(dir.join("complete"), b"complete\n").unwrap();
        let manifest = crate::target_seed::Manifest {
            project_key: project_key.into(),
            rustc_vv: "rustc fixture".into(),
            host_triple: "fixture".into(),
            commit: "fixture".into(),
            profile: "dev".into(),
            cargo_profile: provenant_config::LaneBuildSettings::default()
                .cargo_build_profile(&[])
                .expect("default Cargo profile is UTF-8"),
            features: vec![],
            flags: vec![],
        };
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        dir
    }
    #[test]
    fn retention_obeys_count_and_bytes_and_preview() {
        let state = tempfile::tempdir().unwrap();
        let old = generation(state.path(), "001", 3);
        let middle = generation(state.path(), "002", 4);
        let newest = generation(state.path(), "003", 5);
        let cap = allocated_size(&newest).unwrap();
        let expected = allocated_size(&old).unwrap() + allocated_size(&middle).unwrap();
        let preview = collect(state.path(), 2, cap, true).unwrap();
        assert!(old.exists() && middle.exists() && newest.exists());
        let applied = collect(state.path(), 2, cap, false).unwrap();
        assert_eq!(preview.bytes, expected);
        assert_eq!(preview.bytes, applied.bytes);
        assert!(newest.exists() && !middle.exists() && !old.exists());
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn generation_count_is_per_project_while_bytes_remain_global() {
        let state = tempfile::tempdir().unwrap();
        let old_a = generation_for_project(state.path(), "001", 3, "project-a");
        let only_b = generation_for_project(state.path(), "002", 4, "project-b");
        let new_a = generation_for_project(state.path(), "003", 5, "project-a");
        let report = collect(state.path(), 1, u64::MAX, false).unwrap();
        assert_eq!(report.removed, vec![old_a.clone()]);
        assert!(!old_a.exists());
        assert!(only_b.exists() && new_a.exists());

        let another = tempfile::tempdir().unwrap();
        let older = generation_for_project(another.path(), "001", 3, "project-a");
        let newer = generation_for_project(another.path(), "002", 4, "project-b");
        let cap = allocated_size(&newer).unwrap();
        let retained = collect(another.path(), 10, cap, false).unwrap();
        assert!(retained.removed.is_empty());
        assert!(
            older.exists() && newer.exists(),
            "each project's newest seed stays warm even over the global cap"
        );
    }
    #[test]
    fn active_clone_lock_prevents_collection() {
        let state = tempfile::tempdir().unwrap();
        let generation = generation(state.path(), "001", 3);
        let guard = crate::target_seed::lock_store(state.path()).unwrap();
        assert!(!collect(state.path(), 0, 0, false).unwrap().busy.is_empty());
        assert!(generation.exists());
        drop(guard);
        let report = collect(state.path(), 0, 0, false).unwrap();
        assert!(report.removed.is_empty());
        assert!(
            generation.exists(),
            "the newest generation survives even over cap"
        );
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
