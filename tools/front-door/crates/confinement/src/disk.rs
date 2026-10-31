//! Read-only lane-cache and worktree disk inventory.
pub use provenant_os::fs::available_bytes_at;

/// Sample the existing volume containing a prospective output path.
/// A missing cache directory is not a failed disk observation.
pub fn available_bytes_at_existing(path: &std::path::Path) -> std::io::Result<u64> {
    let mut path = path;
    loop {
        match available_bytes_at(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                path = path.parent().ok_or(error)?;
            }
            result => return result,
        }
    }
}

/// The tightest output volume for lane admission, build pauses and settlement.
/// Prospective cache paths are sampled at their nearest existing ancestor.
pub fn lane_available_bytes(state: &Path, home: &Path, cwd: &Path) -> std::io::Result<u64> {
    lane_available_bytes_with(state, home, cwd, available_bytes_at_existing)
}

fn lane_available_bytes_with(
    state: &Path,
    home: &Path,
    cwd: &Path,
    mut sample: impl FnMut(&Path) -> std::io::Result<u64>,
) -> std::io::Result<u64> {
    let cache = home.join("Library/Caches/provenant-lane-cargo");
    [state, cwd, cache.as_path()]
        .into_iter()
        .try_fold(u64::MAX, |free, path| Ok(free.min(sample(path)?)))
}

use crate::cache::{
    CARGO_CACHE_OWNER, CacheGcPolicy, CacheGcReason, cache_gc_reason_with_newest,
    cache_owner_fingerprint, cache_tombstone, is_cache_digest, read_cache_owner,
};
use crate::permissions::PermissionError;
use crate::repo::VerifiedRepo;
use crate::utility::{GitRead, UtilityBudget, UtilityRuntime, run_utility};
use crate::worktrees::{
    WorkerObserver, WorkerState, WorktreeCondition, WorktreeError, WorktreeInfo, WriterState,
    combine_worker_state, disk_writer_state, parse_worktree_list,
};
use provenant_core::CancelToken;
use provenant_os::{
    disk::DiskReport,
    exec::{Executor, Termination},
    fs::{Directory, FileKind, effective_uid, entry_names},
};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Inspect caches without invoking GC or opening a write lock.
pub fn lane_caches(home: &Path, policy: &CacheGcPolicy, report: &mut DiskReport) {
    let path = home.join("Library/Caches/provenant-lane-cargo");
    let root = match Directory::open_anchor(&path) {
        Ok(root) => root,
        Err(error) if error.is_not_found() => return,
        Err(error) => {
            report.error(&path, error.to_string());
            return;
        }
    };
    let names = match entry_names(&root, 100_000) {
        Ok(names) => names,
        Err(error) => {
            report.error(&path, error.to_string());
            return;
        }
    };
    for name in names {
        let path = path.join(&name);
        let metadata = match root.metadata_at(&name, false) {
            Ok(metadata) => metadata,
            Err(error) => {
                report.error(&path, error.to_string());
                continue;
            }
        };
        if metadata.kind != FileKind::Directory {
            report.error(&path, "lane cache is not a directory");
            continue;
        }
        let errors_before = report.errors.len();
        let Some(index) = report.measure_lane_at(&root, &name, &path) else {
            continue;
        };
        let newest = report.entries[index].newest_write_ns;
        let classified = if report.errors.len() != errors_before
            || metadata.uid != effective_uid()
            || metadata.mode & 0o7777 != 0o700
        {
            Ok(("refused", None))
        } else {
            match root.open_verified_directory(&name) {
                Ok(cache) => match cache.metadata() {
                    Ok(opened) if metadata.same_identity(&opened) => {
                        classify_lane_cache(&cache, Path::new(&name), policy, newest)
                    }
                    Ok(_) => Err(PermissionError::AnchorMismatch),
                    Err(error) => {
                        report.error(&path, error.to_string());
                        Ok(("refused", None))
                    }
                },
                Err(error) => {
                    report.error(&path, error.to_string());
                    Ok(("refused", None))
                }
            }
        };
        let (state, owner) = match classified {
            Ok(value) => value,
            Err(error) => {
                report.error(&path, error.to_string());
                ("refused", None)
            }
        };
        let entry = &mut report.entries[index];
        entry.state = Some(state.into());
        entry.owner = owner.map(|path| path.to_string_lossy().into_owned());
        if matches!(state, "orphaned" | "idle" | "tombstone") {
            entry.reclaim = Some("gc".into());
        }
    }
}

fn classify_lane_cache(
    cache: &Directory,
    relative: &Path,
    policy: &CacheGcPolicy,
    newest_write_ns: i128,
) -> Result<(&'static str, Option<PathBuf>), PermissionError> {
    let name = relative.file_name().and_then(OsStr::to_str).unwrap_or("");
    let tombstone = cache_tombstone(name);
    if tombstone.is_none() && !is_cache_digest(name) {
        return Ok(("unmarked", None));
    }
    let owner = read_cache_owner(cache, relative)?;
    if let Some((device, inode, fingerprint)) = tombstone {
        let metadata = cache
            .metadata()
            .map_err(|_| PermissionError::AnchorMismatch)?;
        if metadata.device != device
            || metadata.inode != inode
            || owner
                .as_ref()
                .is_some_and(|owner| cache_owner_fingerprint(owner) != fingerprint)
        {
            return Ok(("refused", owner.map(|owner| owner.path)));
        }
        if owner.is_none() {
            match cache.metadata_at(OsStr::new(CARGO_CACHE_OWNER), false) {
                Err(error) if error.is_not_found() => {}
                _ => return Ok(("refused", None)),
            }
        }
        return Ok(("tombstone", owner.map(|owner| owner.path)));
    }
    let Some(owner) = owner else {
        let marker = OsStr::new(CARGO_CACHE_OWNER);
        let metadata = match cache.metadata_at(marker, false) {
            Ok(metadata) => metadata,
            Err(error) if error.is_not_found() => return Ok(("unmarked", None)),
            Err(_) => return Ok(("refused", None)),
        };
        if metadata.kind != FileKind::Regular
            || metadata.links != 1
            || metadata.uid != effective_uid()
            || metadata.size > 8192
        {
            return Ok(("refused", None));
        }
        return Ok(("refused", None));
    };
    let state = match cache_gc_reason_with_newest(relative, &owner, policy, || Ok(newest_write_ns))?
    {
        Some(CacheGcReason::Orphaned) => "orphaned",
        Some(CacheGcReason::Idle) => "idle",
        Some(CacheGcReason::FreeFloor) => "free_floor",
        Some(CacheGcReason::Tombstone) => "tombstone",
        Some(CacheGcReason::Foreign) => "foreign",
        Some(CacheGcReason::Live) => "live",
        Some(CacheGcReason::Busy) => "busy",
        Some(CacheGcReason::Unobservable) => "unobservable",
        None => "live",
    };
    Ok((state, Some(owner.path)))
}

/// Inventory worktrees with bounded Git reads and a native writer census.
/// Unobservable exclusion state remains protected. No state or locks are created.
pub fn worktrees(
    repo: &VerifiedRepo,
    utility: &UtilityRuntime,
    executor: &dyn Executor,
    budget: UtilityBudget,
    cancel: &CancelToken,
    report: &mut DiskReport,
    custody: (Option<&Directory>, &dyn WorkerObserver),
) -> Result<Vec<WorktreeInfo>, WorktreeError> {
    let (registration, workers) = custody;
    let outcome = run_utility(
        executor,
        GitRead::WorktreeList,
        repo,
        utility,
        budget,
        cancel,
    )?;
    if outcome.termination != Termination::Exit(0) {
        return Err(WorktreeError::InvalidOutput);
    }
    let mut listed = parse_worktree_list(&outcome.stdout.bytes)?;
    // Git lists the main worktree first, also when invoked in a linked worktree.
    let integration = listed
        .iter()
        .find(|entry| !entry.bare)
        .map(|entry| (entry.path.clone(), entry.head));
    // Attribute nested linked worktrees before their containing primary tree.
    listed.sort_by_key(|entry| std::cmp::Reverse(entry.path.components().count()));
    let roots: Vec<_> = listed
        .iter()
        .filter(|entry| !entry.bare)
        .map(|entry| entry.path.clone())
        .collect();
    let observed = workers.observe_roots(&roots);
    let mut inventory = Vec::new();
    for listed in listed.into_iter().filter(|entry| !entry.bare) {
        let registered = registration.map_or(WorkerState::Unknown, |directory| {
            disk_writer_state(directory, &listed.path)
        });
        let writer = combine_worker_state(
            registered,
            observed
                .get(&listed.path)
                .copied()
                .unwrap_or(WorkerState::Unknown),
        );
        let info = WorktreeInfo {
            path: listed.path,
            branch: listed.branch.map(|branch| branch.as_str().to_owned()),
            head: listed.head,
            dirty: WorktreeCondition::Unknown,
            writer,
        };
        let first = report.entries.len();
        if let Some(index) = report.measure("worktree", &info.path) {
            let merged = integration
                .as_ref()
                .filter(|(main, _)| main != &info.path)
                .and_then(|(_, tip)| {
                    match run_utility(
                        executor,
                        GitRead::MergeBaseIsAncestor {
                            ancestor: info.head,
                            tip: *tip,
                        },
                        repo,
                        utility,
                        budget,
                        cancel,
                    ) {
                        Ok(outcome) if outcome.termination == Termination::Exit(0) => Some(true),
                        Ok(outcome) if outcome.termination == Termination::Exit(1) => Some(false),
                        Ok(_) => {
                            report.error(&info.path, "cannot determine merge containment");
                            None
                        }
                        Err(error) => {
                            report.error(&info.path, error.to_string());
                            None
                        }
                    }
                });
            let entry = &mut report.entries[index];
            entry.branch = info.branch.clone();
            entry.writer = Some(
                match writer {
                    WriterState::Live => "live",
                    WriterState::Busy => "busy",
                    WriterState::Free => "free",
                    WriterState::Unknown => "unknown",
                }
                .into(),
            );
            entry.merged = merged;
            let writer = entry.writer.clone();
            for target in &mut report.entries[first..] {
                if target.class == "host_target" && target.path.starts_with(&info.path) {
                    target.owner = Some(info.path.display().to_string());
                    target.writer = writer.clone();
                }
            }
        }
        inventory.push(info);
    }
    Ok(inventory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CARGO_CACHE_OWNER_PREFIX;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn lane_sample_uses_each_output_volume_and_propagates_errors()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = Path::new("/state");
        let home = Path::new("/home");
        let cwd = Path::new("/checkout");
        let cache = home.join("Library/Caches/provenant-lane-cargo");
        for low in [state, cwd, cache.as_path()] {
            let mut observed = Vec::new();
            assert_eq!(
                lane_available_bytes_with(state, home, cwd, |path| {
                    observed.push(path.to_owned());
                    Ok(if path == low { 99 } else { 100 })
                })?,
                99
            );
            assert_eq!(observed, [state, cwd, cache.as_path()]);
        }
        let error = lane_available_bytes_with(state, home, cwd, |path| {
            if path == cwd {
                Err(std::io::Error::other("volume unavailable"))
            } else {
                Ok(100)
            }
        })
        .err()
        .ok_or("missing sample error")?;
        assert_eq!(error.to_string(), "volume unavailable");
        Ok(())
    }

    fn marker(path: &Path, owner: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let metadata = std::fs::metadata(owner)?;
        let mut bytes = CARGO_CACHE_OWNER_PREFIX.to_vec();
        bytes.extend_from_slice(&metadata.dev().to_le_bytes());
        bytes.extend_from_slice(&metadata.ino().to_le_bytes());
        bytes.extend_from_slice(owner.as_os_str().as_encoded_bytes());
        std::fs::write(path.join(CARGO_CACHE_OWNER), bytes)?;
        Ok(())
    }

    #[test]
    fn lane_inventory_refuses_incomplete_measurements() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let cache = fixture
            .path()
            .join("Library/Caches/provenant-lane-cargo")
            .join("a".repeat(64));
        let owner = fixture.path().join("owner");
        std::fs::create_dir_all(&cache)?;
        std::fs::create_dir(&owner)?;
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))?;
        marker(&cache, &owner)?;
        let mut deep = cache.clone();
        for _ in 0..257 {
            deep.push("d");
            std::fs::create_dir(&deep)?;
        }
        let policy = CacheGcPolicy {
            idle_before_ns: Some(i128::MAX),
            project_owners: [owner].into(),
            ..CacheGcPolicy::default()
        };
        let mut report = DiskReport::new(fixture.path().into(), 0);
        lane_caches(fixture.path(), &policy, &mut report);
        assert!(!report.errors.is_empty());
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].state.as_deref(), Some("refused"));
        assert_eq!(report.entries[0].reclaim, None);
        Ok(())
    }

    #[test]
    fn lane_inventory_can_classify_a_measured_target_as_idle()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let cache = fixture
            .path()
            .join("Library/Caches/provenant-lane-cargo")
            .join("a".repeat(64));
        let owner = fixture.path().join("owner");
        std::fs::create_dir_all(cache.join("target"))?;
        std::fs::create_dir(&owner)?;
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))?;
        marker(&cache, &owner)?;
        std::fs::write(cache.join("target/payload"), vec![1; 8192])?;
        let policy = CacheGcPolicy {
            idle_before_ns: Some(i128::MAX),
            project_owners: [owner].into(),
            ..CacheGcPolicy::default()
        };
        let mut report = DiskReport::new(fixture.path().into(), 0);
        lane_caches(fixture.path(), &policy, &mut report);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].state.as_deref(), Some("idle"));
        assert_eq!(report.entries[0].reclaim.as_deref(), Some("gc"));
        assert!(report.entries[0].parts.as_ref().ok_or("parts")?.target > 0);
        Ok(())
    }

    #[test]
    fn lane_classification_reuses_gc_owner_and_reason() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let owner = fixture.path().join("owner");
        let cache = fixture.path().join("a".repeat(64));
        std::fs::create_dir(&owner)?;
        std::fs::create_dir(&cache)?;
        std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))?;
        marker(&cache, &owner)?;
        let directory = Directory::open_anchor(&cache)?;
        let relative = Path::new(cache.file_name().ok_or("cache name")?);
        let mut policy = CacheGcPolicy {
            idle_before_ns: Some(100),
            ..CacheGcPolicy::default()
        };
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?.0,
            "foreign"
        );
        policy.project_owners.insert(owner.clone());
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?.0,
            "idle"
        );
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 100)?.0,
            "live"
        );
        policy
            .protected_owners
            .insert(owner.clone(), crate::worktrees::WriterState::Live);
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?.0,
            "live"
        );
        std::fs::remove_dir(&owner)?;
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?.0,
            "orphaned"
        );
        Ok(())
    }

    #[test]
    fn lane_classification_refuses_unmarked_and_obsolete_markers()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let cache = fixture.path().join("a".repeat(64));
        std::fs::create_dir(&cache)?;
        let directory = Directory::open_anchor(&cache)?;
        let relative = Path::new(cache.file_name().ok_or("cache name")?);
        let policy = CacheGcPolicy::default();
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?.0,
            "unmarked"
        );
        std::fs::write(
            cache.join(CARGO_CACHE_OWNER),
            b"provenant-lane-cargo-owner-v1\n",
        )?;
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?.0,
            "refused"
        );
        let owner = fixture.path().join("owner");
        std::fs::write(
            cache.join(CARGO_CACHE_OWNER),
            [
                b"provenant-lane-cargo-owner-v1\n",
                owner.as_os_str().as_encoded_bytes(),
            ]
            .concat(),
        )?;
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?,
            ("refused", None)
        );
        std::fs::create_dir(&owner)?;
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?,
            ("refused", None)
        );
        let policy = CacheGcPolicy {
            project_owners: [owner.clone()].into(),
            idle_before_ns: Some(100),
            ..policy
        };
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?,
            ("refused", None)
        );
        std::fs::write(cache.join(CARGO_CACHE_OWNER), b"invalid marker")?;
        assert_eq!(
            classify_lane_cache(&directory, relative, &policy, 0)?.0,
            "refused"
        );
        assert_eq!(
            classify_lane_cache(&directory, Path::new("unknown"), &policy, 0)?.0,
            "unmarked"
        );
        Ok(())
    }

    #[test]
    fn lane_classification_validates_detached_tombstone_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let owner = fixture.path().join("owner");
        let cache = fixture.path().join("cache");
        std::fs::create_dir(&owner)?;
        std::fs::create_dir(&cache)?;
        marker(&cache, &owner)?;
        let directory = Directory::open_anchor(&cache)?;
        let metadata = directory.metadata()?;
        let owner = read_cache_owner(&directory, Path::new("cache"))?.ok_or("owner")?;
        let name = format!(
            ".gc-{:016x}{:016x}{}-1",
            metadata.device,
            metadata.inode,
            cache_owner_fingerprint(&owner)
        );
        let policy = CacheGcPolicy::default();
        assert_eq!(
            classify_lane_cache(&directory, Path::new(&name), &policy, 0)?.0,
            "tombstone"
        );
        let wrong = format!(
            ".gc-{:016x}{:016x}{}-1",
            metadata.device,
            metadata.inode + 1,
            cache_owner_fingerprint(&owner)
        );
        assert_eq!(
            classify_lane_cache(&directory, Path::new(&wrong), &policy, 0)?.0,
            "refused"
        );
        Ok(())
    }
}
