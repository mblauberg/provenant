//! Lane cache collection.

use crate::permissions::{PermissionError, validate_absolute};
use crate::worktrees::{WorkerState, WriterState};
use provenant_os::fs::{Directory, FileKind, FsError, LockGuard, PayloadTree};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub(crate) const CARGO_CACHE_OWNER: &str = ".provenant-worktree-owner";
pub(crate) const CARGO_CACHE_OWNER_PREFIX: &[u8] = b"provenant-lane-cargo-owner-v2\n";
const MAX_CACHE_SWEEP_DEPTH: usize = 32;
const MAX_CACHE_DIRECTORY_ENTRIES: usize = 100_000;

/// Why a lane cache was removed, would be removed or was kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheGcReason {
    /// The recorded owner path no longer exists.
    Orphaned,
    /// Nothing under the cache was written since the idle threshold.
    Idle,
    /// Additional removal to reach the requested available disk space.
    FreeFloor,
    /// A detached cache left by an interrupted sweep.
    Tombstone,
    /// An existing owner outside the selected project; always kept.
    Foreign,
    /// The owner's writer lock is held; always kept.
    Live,
    /// Writer exclusion cannot be established; always kept.
    Unobservable,
}

/// What a lane cache sweep removes besides orphans.
#[derive(Clone, Debug, Default)]
pub struct CacheGcPolicy {
    /// Also remove caches whose newest write is older than this many nanoseconds
    /// since the epoch. `None` removes orphans only.
    pub idle_before_ns: Option<i128>,
    /// After the usual pass, collect oldest caches until this floor is met.
    pub free_floor_bytes: Option<u64>,
    /// Report without deleting.
    pub dry_run: bool,
    /// Canonical worktrees listed by the selected project.
    pub project_owners: BTreeSet<PathBuf>,
    /// Limit collection to listed owners, including orphan and detached output.
    pub selected_only: bool,
    /// Listing observations for canonical owners with live or unknown writers;
    /// never removed or re-probed during the sweep.
    pub protected_owners: BTreeMap<PathBuf, WriterState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CacheGcEntry {
    pub path: PathBuf,
    pub bytes: u64,
    pub reason: CacheGcReason,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CacheGcReport {
    pub dry_run: bool,
    pub removed: Vec<CacheGcEntry>,
    /// Caches kept because ownership or writer state prevents removal.
    pub kept: Vec<CacheGcEntry>,
    pub bytes_freed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_bytes_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_bytes_after: Option<u64>,
    /// Dry-run free space predictions include estimated reclaimable bytes.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub free_bytes_estimated: bool,
}

/// A failed sweep still reports completed removals and bytes already freed.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct CacheGcError {
    pub report: CacheGcReport,
    #[source]
    pub source: Box<PermissionError>,
}

/// Remove marked orphan caches and retry detached remnants from earlier sweeps.
pub fn prune_orphaned_worktree_cargo_caches(home: &Path) -> Result<usize, CacheGcError> {
    gc_worktree_cargo_caches(home, &CacheGcPolicy::default()).map(|report| report.removed.len())
}

/// Remove only the cache derived from a worktree before its removal. The caller
/// holds that worktree's writer lock until this returns. Other orphaned caches
/// and interrupted GC tombstones are left for `provenant gc`.
pub(crate) fn prune_worktree_cargo_cache(home: &Path, cache: &Path) -> Result<(), PermissionError> {
    validate_absolute(home)?;
    let cache_root = home.join("Library/Caches/provenant-lane-cargo");
    if cache.parent() != Some(cache_root.as_path()) {
        return Err(PermissionError::InvalidPath(cache.to_owned()));
    }
    let name = cache
        .file_name()
        .ok_or_else(|| PermissionError::InvalidPath(cache.to_owned()))?;
    if !name.to_str().is_some_and(is_cache_digest) {
        return Err(PermissionError::InvalidPath(cache.to_owned()));
    }
    let Some(root) = open_cache_root(&cache_root)? else {
        return Ok(());
    };
    prune_worktree_cache(
        &root,
        &cache_root,
        name,
        &CacheGcPolicy::default(),
        &mut CacheGcReport::default(),
        &mut |_| Err(WorkerState::Unknown),
        false,
    )
}

/// Sweep the lane cache root: orphaned caches always, idle ones when the policy
/// sets a threshold and lists the owner in this project. Retry GC tombstones
/// too. Without a writer guard, idle and free-floor caches are kept as
/// unobservable.
/// Directories are opened without following symlinks or crossing devices.
pub fn gc_worktree_cargo_caches(
    home: &Path,
    policy: &CacheGcPolicy,
) -> Result<CacheGcReport, CacheGcError> {
    gc_worktree_cargo_caches_with_writer_guard(home, policy, |_| Err(WorkerState::Unknown))
}

type CacheWriterGuard<'a> = dyn FnMut(&Path) -> Result<LockGuard, WorkerState> + 'a;

/// Sweep caches while holding the owner's writer lock across idle or free-floor
/// detachment.
/// The guard must exclude writers and refuse missing or unobservable locks.
pub fn gc_worktree_cargo_caches_with_writer_guard(
    home: &Path,
    policy: &CacheGcPolicy,
    writer_guard: impl FnMut(&Path) -> Result<LockGuard, WorkerState>,
) -> Result<CacheGcReport, CacheGcError> {
    gc_caches_with_free_space(home, policy, writer_guard, |path| {
        provenant_os::fs::Directory::open_anchor(path)
            .map_err(std::io::Error::other)?
            .available_bytes()
    })
}

fn gc_caches_with_free_space(
    home: &Path,
    policy: &CacheGcPolicy,
    mut writer_guard: impl FnMut(&Path) -> Result<LockGuard, WorkerState>,
    mut free_space: impl FnMut(&Path) -> std::io::Result<u64>,
) -> Result<CacheGcReport, CacheGcError> {
    let mut report = CacheGcReport {
        dry_run: policy.dry_run,
        ..CacheGcReport::default()
    };
    let sweep = (|| {
        validate_absolute(home)?;
        let cache_root = home.join("Library/Caches/provenant-lane-cargo");
        let Some(root) = open_cache_root(&cache_root)? else {
            return Ok(());
        };
        if policy.free_floor_bytes.is_some() {
            report.free_bytes_before = Some(
                free_space(&cache_root)
                    .map_err(|error| cache_io_error("read free space", &cache_root, error))?,
            );
            report.free_bytes_after = report.free_bytes_before;
            report.free_bytes_estimated = policy.dry_run;
        }
        sweep_cache_root(&root, &cache_root, policy, &mut report, &mut writer_guard)?;
        if let Some(floor) = policy.free_floor_bytes {
            let mut available = if policy.dry_run {
                report
                    .free_bytes_before
                    .unwrap_or(0)
                    .saturating_add(report.bytes_freed)
            } else {
                free_space(&cache_root)
                    .map_err(|error| cache_io_error("read free space", &cache_root, error))?
            };
            report.free_bytes_after = Some(available);
            if available < floor {
                let mut pressure = policy.clone();
                pressure.idle_before_ns = Some(i128::MAX);
                let mut candidates = Vec::new();
                for name in provenant_os::fs::entry_names(&root, MAX_CACHE_DIRECTORY_ENTRIES)
                    .map_err(cache_fs_error("readdir", Path::new(".")))?
                {
                    let path = cache_root.join(&name);
                    if !name.to_str().is_some_and(is_cache_digest)
                        || report
                            .removed
                            .iter()
                            .chain(&report.kept)
                            .any(|entry| entry.path == path)
                    {
                        continue;
                    }
                    let metadata = root
                        .metadata_at(&name, false)
                        .map_err(cache_fs_error("stat", Path::new(&name)))?;
                    if metadata.kind != FileKind::Directory
                        || metadata.uid != provenant_os::fs::effective_uid()
                        || metadata.mode & 0o7777 != 0o700
                    {
                        continue;
                    }
                    let cache = root
                        .open_verified_directory(&name)
                        .map_err(cache_fs_error("open verified directory", Path::new(&name)))?;
                    let Some(owner) = read_cache_owner(&cache, Path::new(&name))? else {
                        continue;
                    };
                    if cache_gc_reason(&cache, Path::new(&name), &owner, &pressure)?
                        == Some(CacheGcReason::Idle)
                    {
                        let (_, newest) = measure_cache(&cache, Path::new(&name), 0)?;
                        candidates.push((newest, name));
                    }
                }
                candidates.sort();
                for (newest, name) in candidates {
                    if available >= floor {
                        break;
                    }
                    // Recheck against the observed last write while holding writer exclusion.
                    pressure.idle_before_ns = Some(newest.saturating_add(1));
                    let before = report.bytes_freed;
                    let removed = report.removed.len();
                    prune_worktree_cache(
                        &root,
                        &cache_root,
                        &name,
                        &pressure,
                        &mut report,
                        &mut writer_guard,
                        true,
                    )?;
                    if report.removed.len() == removed {
                        continue;
                    }
                    available = if policy.dry_run {
                        available.saturating_add(report.bytes_freed.saturating_sub(before))
                    } else {
                        free_space(&cache_root).map_err(|error| {
                            cache_io_error("read free space", &cache_root, error)
                        })?
                    };
                    report.free_bytes_after = Some(available);
                }
            }
        }
        Ok(())
    })();
    match sweep {
        Ok(()) => Ok(report),
        Err(source) => Err(CacheGcError {
            report,
            source: Box::new(source),
        }),
    }
}

fn open_cache_root(
    cache_root: &Path,
) -> Result<Option<provenant_os::fs::Directory>, PermissionError> {
    use std::io;

    match std::fs::symlink_metadata(cache_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Ok(None);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(cache_io_error("stat cache root", Path::new("."), error)),
    }
    let root = Directory::open_anchor(cache_root)
        .map_err(cache_fs_error("open cache root", Path::new(".")))?;
    Ok(Some(root))
}

fn cache_gc_operation<T>(
    event: &str,
    relative: &Path,
    operation: impl FnOnce() -> Result<T, FsError>,
) -> Result<T, FsError> {
    #[cfg(test)]
    return tests::gc_faults::operation(event, relative, operation);
    #[cfg(not(test))]
    {
        let _ = (event, relative);
        operation()
    }
}

fn cache_gc_entries(
    directory: &Directory,
    relative: &Path,
) -> Result<Box<dyn Iterator<Item = Result<OsString, FsError>>>, FsError> {
    let relative = relative.to_owned();
    Ok(Box::new(directory.entries()?.map(move |entry| {
        let name = entry?;
        cache_gc_operation("readdir", &relative.join(&name), || Ok(name))
    })))
}

fn sweep_cache_root(
    root: &provenant_os::fs::Directory,
    cache_root: &Path,
    policy: &CacheGcPolicy,
    report: &mut CacheGcReport,
    writer_guard: &mut CacheWriterGuard<'_>,
) -> Result<(), PermissionError> {
    let mut failures = Vec::new();
    // Snapshot names so newly detached tombstones are retried on a later run.
    let entries: Vec<_> = cache_gc_entries(root, Path::new("."))
        .map_err(cache_fs_error("opendir", Path::new(".")))?
        .take(MAX_CACHE_DIRECTORY_ENTRIES + 1)
        .collect();
    if entries.len() > MAX_CACHE_DIRECTORY_ENTRIES {
        return Err(cache_fs_error("readdir", Path::new("."))(
            FsError::EntryLimit {
                limit: MAX_CACHE_DIRECTORY_ENTRIES,
            },
        ));
    }
    for entry in entries {
        let name = match entry {
            Ok(name) => name,
            Err(error) => {
                failures.push(cache_fs_error("readdir", Path::new("."))(error).to_string());
                continue;
            }
        };
        if let Err(error) =
            prune_worktree_cache(root, cache_root, &name, policy, report, writer_guard, false)
        {
            failures.push(format!("{:?}: {error}", name));
        }
    }
    if !failures.is_empty() {
        return Err(PermissionError::CacheSweepIo(std::io::Error::other(
            failures.join("; "),
        )));
    }
    Ok(())
}

fn prune_worktree_cache(
    root: &provenant_os::fs::Directory,
    cache_root: &Path,
    name: &OsStr,
    policy: &CacheGcPolicy,
    report: &mut CacheGcReport,
    writer_guard: &mut CacheWriterGuard<'_>,
    free_floor: bool,
) -> Result<(), PermissionError> {
    let pressure_reason = |reason| match reason {
        Some(CacheGcReason::Idle) if free_floor => Some(CacheGcReason::FreeFloor),
        other => other,
    };
    let Some(digest) = name.to_str() else {
        return Ok(());
    };
    let tombstone = cache_tombstone(digest);
    if tombstone.is_none() && !is_cache_digest(digest) {
        return Ok(());
    }
    let metadata =
        match cache_gc_operation("stat", Path::new(name), || root.metadata_at(name, false)) {
            Ok(metadata) if metadata.kind == FileKind::Directory => metadata,
            Ok(_) => return Ok(()),
            Err(error) if error.is_not_found() => return Ok(()),
            Err(error) => return Err(cache_fs_error("stat", Path::new(name))(error)),
        };
    if metadata.uid != provenant_os::fs::effective_uid() || metadata.mode & 0o7777 != 0o700 {
        return Ok(());
    }
    let cache = match cache_gc_operation("open verified", Path::new(name), || {
        root.open_verified_directory(name)
    }) {
        Ok(cache) => cache,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => {
            return Err(cache_fs_error("open verified directory", Path::new(name))(
                error,
            ));
        }
    };
    let opened = cache
        .metadata()
        .map_err(cache_fs_error("fstat", Path::new(name)))?;
    if !metadata.same_identity(&opened) {
        return Err(PermissionError::AnchorMismatch);
    }
    verify_cache_metadata(&opened)?;
    let owner = read_cache_owner(&cache, Path::new(name))?;
    if policy.selected_only
        && owner
            .as_ref()
            .is_none_or(|owner| !policy.project_owners.contains(&owner.path))
    {
        return Ok(());
    }
    let reason = if let Some((device, inode, fingerprint)) = tombstone {
        // Bind retry authority to what GC intended to rename, even if a
        // process stopped before verification or quarantine could complete.
        if opened.device != device
            || opened.inode != inode
            || owner
                .as_ref()
                .is_some_and(|owner| cache_owner_fingerprint(owner) != fingerprint)
        {
            return Err(PermissionError::AnchorMismatch);
        }
        // A crash after marker unlink but before rmdir can leave a remnant.
        // Tombstone names reserve detached directories for collection, even
        // if another held writer added a file after marker deletion.
        if owner.is_none() {
            match cache_gc_operation("stat", &Path::new(name).join(CARGO_CACHE_OWNER), || {
                cache.metadata_at(OsStr::new(CARGO_CACHE_OWNER), false)
            }) {
                Err(error) if error.is_not_found() => {}
                Ok(_) => return Ok(()),
                Err(error) => {
                    return Err(cache_fs_error(
                        "stat",
                        &Path::new(name).join(CARGO_CACHE_OWNER),
                    )(error));
                }
            }
        }
        CacheGcReason::Tombstone
    } else {
        let Some(owner) = owner.as_ref() else {
            return Ok(());
        };
        let Some(reason) =
            pressure_reason(cache_gc_reason(&cache, Path::new(name), owner, policy)?)
        else {
            if let Some(state) = policy.protected_owners.get(&owner.path) {
                report.kept.push(CacheGcEntry {
                    path: cache_root.join(name),
                    bytes: 0,
                    reason: match state {
                        WriterState::Live => CacheGcReason::Live,
                        _ => CacheGcReason::Unobservable,
                    },
                });
            }
            return Ok(());
        };
        if reason == CacheGcReason::Foreign {
            report.kept.push(CacheGcEntry {
                path: cache_root.join(name),
                bytes: 0,
                reason,
            });
            return Ok(());
        }
        reason
    };
    let (bytes, _) = measure_cache(&cache, Path::new(name), 0)?;
    if policy.dry_run {
        if matches!(reason, CacheGcReason::Idle | CacheGcReason::FreeFloor) {
            let owner = owner.as_ref().ok_or(PermissionError::AnchorMismatch)?;
            // Probe only: a dry run never needs to hold writer exclusion.
            if let Err(state) = writer_guard(&owner.path) {
                report.kept.push(CacheGcEntry {
                    path: cache_root.join(name),
                    bytes: 0,
                    reason: if state == WorkerState::Live {
                        CacheGcReason::Live
                    } else {
                        CacheGcReason::Unobservable
                    },
                });
                return Ok(());
            }
        }
        report.bytes_freed = report.bytes_freed.saturating_add(bytes);
    } else {
        let detached;
        let removal_name = if tombstone.is_some() {
            name
        } else {
            let original_owner = owner.as_ref().ok_or(PermissionError::AnchorMismatch)?;
            detached = OsString::from(format!(
                ".gc-{:016x}{:016x}{}-{}",
                opened.device,
                opened.inode,
                cache_owner_fingerprint(original_owner),
                std::process::id()
            ));
            let idle_guard = if matches!(reason, CacheGcReason::Idle | CacheGcReason::FreeFloor) {
                match writer_guard(&original_owner.path) {
                    Ok(guard) => Some(guard),
                    Err(state) => {
                        report.kept.push(CacheGcEntry {
                            path: cache_root.join(name),
                            bytes: 0,
                            reason: if state == WorkerState::Live {
                                CacheGcReason::Live
                            } else {
                                CacheGcReason::Unobservable
                            },
                        });
                        return Ok(());
                    }
                }
            } else {
                None
            };
            // Measurement may take time. Recheck the marker, owner and last
            // write immediately before detaching the cache from the lane path.
            if read_cache_owner(&cache, Path::new(name))? != owner {
                return Ok(());
            }
            let current_reason = pressure_reason(cache_gc_reason(
                &cache,
                Path::new(name),
                original_owner,
                policy,
            )?);
            if current_reason == Some(CacheGcReason::Foreign) {
                report.kept.push(CacheGcEntry {
                    path: cache_root.join(name),
                    bytes: 0,
                    reason: CacheGcReason::Foreign,
                });
            }
            if current_reason != Some(reason) {
                return Ok(());
            }
            cache_gc_operation("before rename", Path::new(name), || {
                root.rename_noreplace(name, root, &detached)
            })
            .map_err(cache_fs_error("rename to tombstone", Path::new(name)))?;
            // The lane path is now free for a writer to create a fresh cache.
            drop(idle_guard);
            detached.as_os_str()
        };
        let detached_cache = match verify_detached_cache(root, removal_name, &opened, &owner) {
            Ok(cache) => cache,
            Err(error) => {
                // A refused replacement is not an authorised tombstone.
                // Quarantine it so another sweep cannot turn refusal into
                // deletion. Never replace a freshly created lane path.
                let rejected =
                    OsString::from(format!(".gc-refused-{}", removal_name.to_string_lossy()));
                cache_gc_operation("before rename", Path::new(removal_name), || {
                    root.rename_noreplace(removal_name, root, &rejected)
                })
                .map_err(cache_fs_error("quarantine", Path::new(removal_name)))?;
                return Err(error);
            }
        };
        remove_cache_contents(
            &detached_cache,
            Path::new(removal_name),
            &mut report.bytes_freed,
        )?;
        // The top-level marker survives every earlier failure.
        let marker_name = OsStr::new(CARGO_CACHE_OWNER);
        match cache_gc_operation("stat", &Path::new(removal_name).join(marker_name), || {
            detached_cache.metadata_at(marker_name)
        }) {
            Ok(marker) => {
                cache_gc_operation("unlink", &Path::new(removal_name).join(marker_name), || {
                    detached_cache.unlink(marker_name)
                })
                .map_err(cache_fs_error(
                    "unlink",
                    &Path::new(removal_name).join(marker_name),
                ))?;
                report.bytes_freed = report
                    .bytes_freed
                    .saturating_add(marker.allocated.max(marker.size));
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => {
                return Err(cache_fs_error(
                    "stat",
                    &Path::new(removal_name).join(marker_name),
                )(error));
            }
        }
        cache_gc_operation("rmdir", Path::new(removal_name), || {
            root.rmdir(removal_name)
        })
        .map_err(cache_fs_error("rmdir", Path::new(removal_name)))?;
    }
    report.removed.push(CacheGcEntry {
        path: cache_root.join(name),
        bytes,
        reason,
    });
    Ok(())
}

fn verify_detached_cache(
    root: &provenant_os::fs::Directory,
    name: &OsStr,
    expected: &provenant_os::fs::Metadata,
    owner: &Option<CacheOwner>,
) -> Result<PayloadTree, PermissionError> {
    // The held original descriptor alone cannot identify what was renamed.
    let after = cache_gc_operation("stat", Path::new(name), || root.metadata_at(name, false))
        .map_err(cache_fs_error("stat", Path::new(name)))?;
    verify_cache_metadata(&after)?;
    if !expected.same_identity(&after) {
        return Err(PermissionError::AnchorMismatch);
    }
    // Only a verified tombstone may dispose of lock-named cache payload.
    // Verified-open failures keep their operation and path.
    let cache = cache_gc_operation("open verified", Path::new(name), || {
        PayloadTree::claim(root, name, expected)
    })
    .map_err(|error| match error {
        FsError::PayloadMismatch { .. } | FsError::NotPrivate { .. } => {
            PermissionError::AnchorMismatch
        }
        error => cache_fs_error("open verified directory", Path::new(name))(error),
    })?;
    let verified = cache
        .metadata()
        .map_err(cache_fs_error("fstat", Path::new(name)))?;
    verify_cache_metadata(&verified)?;
    if !expected.same_identity(&verified)
        || &read_cache_owner(cache.directory(), Path::new(name))? != owner
    {
        return Err(PermissionError::AnchorMismatch);
    }
    Ok(cache)
}

pub fn is_cache_digest(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn cache_tombstone(name: &str) -> Option<(u64, u64, &str)> {
    let (hex, pid) = name
        .strip_prefix(".gc-")
        .and_then(|rest| rest.split_once('-'))?;
    if !is_cache_digest(hex) || pid.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((
        u64::from_str_radix(&hex[..16], 16).ok()?,
        u64::from_str_radix(&hex[16..32], 16).ok()?,
        &hex[32..],
    ))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CacheOwner {
    pub(crate) path: PathBuf,
    pub(crate) identity: (u64, u64),
}

pub(crate) fn cache_owner_fingerprint(owner: &CacheOwner) -> String {
    let mut digest = Sha256::new();
    digest.update(CARGO_CACHE_OWNER_PREFIX);
    digest.update(owner.identity.0.to_le_bytes());
    digest.update(owner.identity.1.to_le_bytes());
    digest.update(owner.path.as_os_str().as_encoded_bytes());
    provenant_core::hex(&digest.finalize()[..16])
}

pub(crate) fn read_cache_owner(
    cache: &provenant_os::fs::Directory,
    relative: &Path,
) -> Result<Option<CacheOwner>, PermissionError> {
    let marker_name = OsStr::new(CARGO_CACHE_OWNER);
    let marker_path = relative.join(marker_name);
    let marker_meta = match cache_gc_operation("stat", &relative.join(marker_name), || {
        cache.metadata_at(marker_name, false)
    }) {
        Ok(metadata)
            if metadata.kind == FileKind::Regular
                && metadata.links == 1
                && metadata.uid == provenant_os::fs::effective_uid() =>
        {
            metadata
        }
        Ok(_) => return Ok(None),
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(cache_fs_error("stat", &marker_path)(error)),
    };
    if marker_meta.size > 8192 {
        return Ok(None);
    }
    let mut marker = Vec::with_capacity(marker_meta.size as usize);
    let file = match cache_gc_operation("read", &relative.join(marker_name), || {
        cache.open_read(marker_name)
    }) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(cache_fs_error("open marker", &marker_path)(error)),
    };
    if let Err(error) = file.take(8193).read_to_end(&mut marker) {
        return Err(cache_io_error("read marker", &marker_path, error));
    }
    if marker.len() > 8192 {
        return Ok(None);
    }
    let Some(owner_bytes) = marker.strip_prefix(CARGO_CACHE_OWNER_PREFIX) else {
        return Ok(None);
    };
    // Short payloads cannot authorise collection.
    if owner_bytes.len() <= 16 {
        return Ok(None);
    }
    let (identity, path_bytes) = owner_bytes.split_at(16);
    let device = u64::from_le_bytes(
        identity[..8]
            .try_into()
            .map_err(|_| PermissionError::InvalidFixture)?,
    );
    let inode = u64::from_le_bytes(
        identity[8..]
            .try_into()
            .map_err(|_| PermissionError::InvalidFixture)?,
    );
    if path_bytes.is_empty() {
        return Ok(None);
    }
    let owner = PathBuf::from(OsString::from_vec(path_bytes.to_vec()));
    if !owner.is_absolute()
        || owner
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Ok(None);
    }
    Ok(Some(CacheOwner {
        path: owner,
        identity: (device, inode),
    }))
}

fn cache_gc_reason(
    cache: &provenant_os::fs::Directory,
    relative: &Path,
    owner: &CacheOwner,
    policy: &CacheGcPolicy,
) -> Result<Option<CacheGcReason>, PermissionError> {
    cache_gc_reason_with_newest(relative, owner, policy, || {
        measure_cache(cache, relative, 0).map(|(_, newest)| newest)
    })
}

/// Share GC eligibility with read-only inventories supplying their own bounded
/// measurement. This helper observes the owner and never mutates the cache.
pub(crate) fn cache_gc_reason_with_newest(
    relative: &Path,
    owner: &CacheOwner,
    policy: &CacheGcPolicy,
    newest_write_ns: impl FnOnce() -> Result<i128, PermissionError>,
) -> Result<Option<CacheGcReason>, PermissionError> {
    use std::io;

    let reason = match std::fs::symlink_metadata(&owner.path) {
        Ok(_) if !policy.project_owners.contains(&owner.path) => CacheGcReason::Foreign,
        Ok(_) if policy.protected_owners.contains_key(&owner.path) => return Ok(None),
        Ok(metadata) if owner.identity == (metadata.dev(), metadata.ino()) => {
            let Some(threshold) = policy.idle_before_ns else {
                return Ok(None);
            };
            let newest = newest_write_ns()?;
            if newest >= threshold {
                return Ok(None);
            }
            CacheGcReason::Idle
        }
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => CacheGcReason::Orphaned,
        Err(error) => {
            return Err(cache_fs_error("stat owner", relative)(FsError::Io {
                operation: "symlink_metadata owner",
                path: owner.path.clone(),
                source: error,
            }));
        }
    };
    Ok(Some(reason))
}

/// Disk bytes and the newest modification time (ns since the epoch) under a
/// cache, without following symlinks.
fn measure_cache(
    directory: &provenant_os::fs::Directory,
    relative: &Path,
    depth: usize,
) -> Result<(u64, i128), PermissionError> {
    if depth >= MAX_CACHE_SWEEP_DEPTH {
        return Err(PermissionError::CacheSweepDepth);
    }
    let mut bytes = 0_u64;
    let mut newest = directory
        .metadata()
        .map_err(cache_fs_error("fstat", relative))?
        .modified_ns;
    for entry in
        cache_gc_entries(directory, relative).map_err(cache_fs_error("opendir", relative))?
    {
        let name = entry.map_err(cache_fs_error("readdir", relative))?;
        let path = relative.join(&name);
        let metadata = cache_gc_operation("stat", &relative.join(&name), || {
            directory.metadata_at(&name, false)
        })
        .map_err(cache_fs_error("stat", &path))?;
        bytes = bytes.saturating_add(metadata.allocated.max(metadata.size));
        newest = newest.max(metadata.modified_ns);
        if metadata.kind == FileKind::Directory {
            let child = cache_gc_operation("open verified", &relative.join(&name), || {
                directory.open_verified_directory(&name)
            })
            .map_err(cache_fs_error("open verified directory", &path))?;
            let (child_bytes, child_newest) = measure_cache(&child, &path, depth + 1)?;
            bytes = bytes.saturating_add(child_bytes);
            newest = newest.max(child_newest);
        }
    }
    Ok((bytes, newest))
}

fn remove_cache_contents(
    directory: &PayloadTree,
    relative: &Path,
    bytes_freed: &mut u64,
) -> Result<(), PermissionError> {
    remove_cache_contents_at(directory, relative, 0, bytes_freed)
}

fn cache_fs_error<'a>(
    operation: &'static str,
    path: &'a Path,
) -> impl FnOnce(FsError) -> PermissionError + 'a {
    move |source| PermissionError::CacheSweepFilesystem {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn cache_io_error(operation: &'static str, path: &Path, source: std::io::Error) -> PermissionError {
    cache_fs_error(operation, path)(FsError::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

fn remove_cache_contents_at(
    directory: &PayloadTree,
    relative: &Path,
    depth: usize,
    bytes_freed: &mut u64,
) -> Result<(), PermissionError> {
    if depth >= MAX_CACHE_SWEEP_DEPTH {
        return Err(PermissionError::CacheSweepDepth);
    }
    let names = cache_gc_entries(directory.directory(), relative)
        .map_err(cache_fs_error("readdir", relative))?
        .take(MAX_CACHE_DIRECTORY_ENTRIES + 1)
        .collect::<Result<Vec<_>, _>>()
        .and_then(|names| {
            if names.len() > MAX_CACHE_DIRECTORY_ENTRIES {
                Err(FsError::EntryLimit {
                    limit: MAX_CACHE_DIRECTORY_ENTRIES,
                })
            } else {
                Ok(names)
            }
        })
        .map_err(cache_fs_error("readdir", relative))?;
    for name in names {
        if depth == 0 && name == CARGO_CACHE_OWNER {
            continue;
        }
        let path = relative.join(&name);
        let metadata = cache_gc_operation("stat", &relative.join(&name), || {
            directory.metadata_at(&name)
        })
        .map_err(cache_fs_error("stat", &path))?;
        if metadata.kind == FileKind::Directory {
            let child = cache_gc_operation("open verified", &relative.join(&name), || {
                directory.child(&name)
            })
            .map_err(cache_fs_error("open verified directory", &path))?;
            remove_cache_contents_at(&child, &path, depth + 1, bytes_freed)?;
            drop(child);
            cache_gc_operation("rmdir", &relative.join(&name), || directory.rmdir(&name))
                .map_err(cache_fs_error("rmdir", &path))?;
        } else {
            cache_gc_operation("unlink", &relative.join(&name), || directory.unlink(&name))
                .map_err(cache_fs_error("unlink", &path))?;
        }
        *bytes_freed = bytes_freed.saturating_add(metadata.allocated.max(metadata.size));
    }
    Ok(())
}

pub(crate) fn verify_cache_metadata(
    metadata: &provenant_os::fs::Metadata,
) -> Result<(), PermissionError> {
    if metadata.kind != FileKind::Directory
        || metadata.uid != provenant_os::fs::effective_uid()
        || metadata.mode & 0o7777 != 0o700
    {
        return Err(PermissionError::AnchorMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests use real file timestamps to construct race conditions.
    #![allow(clippy::disallowed_methods)]

    use super::*;
    use provenant_core::CancelToken;

    #[test]
    fn lane_cache_gc_sweep_removes_only_marked_caches_for_missing_worktrees()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temporary = tempfile::tempdir()?;
        let home = temporary.path();
        let cache_root = home.join("Library/Caches/provenant-lane-cargo");
        std::fs::create_dir_all(&cache_root)?;
        let orphan = cache_root.join("a".repeat(64));
        let live = cache_root.join("b".repeat(64));
        let unknown = cache_root.join("c".repeat(64));
        for cache in [&orphan, &live, &unknown] {
            std::fs::create_dir(cache)?;
            std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
        }
        let existing_worktree = home.join("worktree");
        std::fs::create_dir(&existing_worktree)?;
        let old_worktree = home.join("removed-worktree");
        std::fs::create_dir(&old_worktree)?;
        let old_identity = std::fs::metadata(&old_worktree)?;
        std::fs::rename(&old_worktree, home.join("previous-worktree"))?;
        for (cache, owner, identity) in [
            (&orphan, old_worktree, old_identity),
            (
                &live,
                existing_worktree.clone(),
                std::fs::metadata(&existing_worktree)?,
            ),
        ] {
            let mut marker = CARGO_CACHE_OWNER_PREFIX.to_vec();
            marker.extend_from_slice(&identity.dev().to_le_bytes());
            marker.extend_from_slice(&identity.ino().to_le_bytes());
            marker.extend_from_slice(owner.as_os_str().as_encoded_bytes());
            std::fs::write(cache.join(CARGO_CACHE_OWNER), marker)?;
        }
        std::fs::create_dir(orphan.join("release"))?;
        std::fs::write(orphan.join("release/provenant"), b"lane artifact")?;

        assert_eq!(prune_orphaned_worktree_cargo_caches(home)?, 1);
        assert!(!orphan.exists());
        assert!(live.is_dir());
        assert!(unknown.is_dir());
        Ok(())
    }

    #[test]
    fn lane_cache_gc_sweep_skips_deep_cache_and_continues() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir()?;
        let home = temporary.path();
        let cache_root = home.join("Library/Caches/provenant-lane-cargo");
        std::fs::create_dir_all(&cache_root)?;
        let deep = cache_root.join("a".repeat(64));
        let ordinary = cache_root.join("b".repeat(64));
        for cache in [&deep, &ordinary] {
            std::fs::create_dir(cache)?;
            std::fs::set_permissions(cache, std::fs::Permissions::from_mode(0o700))?;
        }
        let missing_owner = home.join("missing-worktree");
        let mut marker = CARGO_CACHE_OWNER_PREFIX.to_vec();
        marker.extend_from_slice(&1_u64.to_le_bytes());
        marker.extend_from_slice(&2_u64.to_le_bytes());
        marker.extend_from_slice(missing_owner.as_os_str().as_encoded_bytes());
        for cache in [&deep, &ordinary] {
            std::fs::write(cache.join(CARGO_CACHE_OWNER), &marker)?;
        }
        let mut nested = deep.clone();
        for _ in 0..MAX_CACHE_SWEEP_DEPTH + 1 {
            nested.push("nested");
            std::fs::create_dir(&nested)?;
        }

        let error = prune_orphaned_worktree_cargo_caches(home)
            .expect_err("deep cache is skipped and reported after the sweep");
        assert!(error.to_string().contains(&"a".repeat(64)));
        assert!(deep.is_dir());
        assert!(!ordinary.exists());
        assert_eq!(error.report.removed.len(), 1);
        assert!(error.report.bytes_freed > 0);
        Ok(())
    }

    fn test_writer_lock(home: &Path) -> Result<provenant_os::fs::Directory, FsError> {
        use provenant_os::fs::{Directory, LockMode};
        let parent = Directory::open_anchor(home)?;
        parent.mkdir(OsStr::new("writer-state"), 0o700)?;
        let directory = parent.open_verified_directory(OsStr::new("writer-state"))?;
        let _guard = directory.try_lock(OsStr::new("writer.lock"), LockMode::Exclusive)?;
        Ok(directory)
    }

    // Exercise races and I/O failures at GC operations without timing-dependent threads.
    pub(super) mod gc_faults {
        use super::*;
        use provenant_core::{Clock, WaitBudget};
        use provenant_os::fs::LockMode;
        use std::{io, os::unix::fs::PermissionsExt, sync::Arc};

        type CacheGcCallback =
            std::sync::Arc<dyn Fn(&str, &Path) -> std::io::Result<()> + Send + Sync>;

        thread_local! {
            static CACHE_GC_EVENT: std::cell::RefCell<Option<(PathBuf, CacheGcCallback)>> =
                const { std::cell::RefCell::new(None) };
        }

        struct CacheGcEvent;
        impl CacheGcEvent {
            fn install(root: PathBuf, event: CacheGcCallback) -> Self {
                CACHE_GC_EVENT.with(|slot| *slot.borrow_mut() = Some((root, event)));
                Self
            }
        }
        impl Drop for CacheGcEvent {
            fn drop(&mut self) {
                CACHE_GC_EVENT.with(|slot| *slot.borrow_mut() = None);
            }
        }

        pub(crate) fn operation<T>(
            event: &str,
            relative: &Path,
            operation: impl FnOnce() -> Result<T, FsError>,
        ) -> Result<T, FsError> {
            cache_gc_event(event, relative)?;
            let result = operation()?;
            if event == "before rename" {
                cache_gc_event("after rename", relative)?;
            }
            Ok(result)
        }

        fn cache_gc_event(event: &str, relative: &Path) -> Result<(), FsError> {
            CACHE_GC_EVENT.with(|slot| {
                let slot = slot.borrow();
                let Some((root, callback)) = slot.as_ref() else {
                    return Ok(());
                };
                let path = root.join(relative);
                callback(event, &path).map_err(|source| {
                    if event == "open verified" {
                        FsError::RaceRefused {
                            name: relative.into(),
                        }
                    } else {
                        FsError::Io {
                            operation: "GC test event",
                            path,
                            source,
                        }
                    }
                })
            })
        }

        type Event = Arc<dyn Fn(&str, &Path) -> io::Result<()> + Send + Sync>;

        fn sweep(
            home: &Path,
            policy: &CacheGcPolicy,
            event: Event,
        ) -> Result<CacheGcReport, CacheGcError> {
            sweep_with_writer_guard(home, policy, event, &mut |_| Err(WorkerState::Unknown))
        }

        fn sweep_with_writer_guard(
            home: &Path,
            policy: &CacheGcPolicy,
            event: Event,
            writer_guard: &mut CacheWriterGuard<'_>,
        ) -> Result<CacheGcReport, CacheGcError> {
            let path = home.join("Library/Caches/provenant-lane-cargo");
            let mut report = CacheGcReport {
                dry_run: policy.dry_run,
                ..CacheGcReport::default()
            };
            let _event = CacheGcEvent::install(path.clone(), event);
            let result = Directory::open_anchor(&path)
                .map_err(PermissionError::Filesystem)
                .and_then(|root| sweep_cache_root(&root, &path, policy, &mut report, writer_guard));
            result
                .map(|()| report.clone())
                .map_err(|source| CacheGcError {
                    report,
                    source: Box::new(source),
                })
        }

        fn cache(home: &Path, name: &str, owner: &Path) -> io::Result<PathBuf> {
            let path = home.join("Library/Caches/provenant-lane-cargo").join(name);
            std::fs::create_dir_all(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            let identity = std::fs::metadata(owner).ok();
            let mut marker = CARGO_CACHE_OWNER_PREFIX.to_vec();
            marker.extend_from_slice(&identity.as_ref().map_or(1, MetadataExt::dev).to_le_bytes());
            marker.extend_from_slice(&identity.as_ref().map_or(2, MetadataExt::ino).to_le_bytes());
            marker.extend_from_slice(owner.as_os_str().as_encoded_bytes());
            std::fs::write(path.join(CARGO_CACHE_OWNER), marker)?;
            std::fs::write(path.join("artifact"), b"cached build")?;
            Ok(path)
        }

        #[test]
        fn free_floor_collects_oldest_first_and_keeps_live_reserved_and_foreign()
        -> Result<(), Box<dyn std::error::Error>> {
            for (dry_run, floor) in [(true, 1), (false, 100), (true, u64::MAX), (false, u64::MAX)] {
                let temporary = tempfile::tempdir()?;
                let home = temporary.path();
                let locks = test_writer_lock(home)?;
                let mut paths = Vec::new();
                let mut owners = Vec::new();
                // Names deliberately disagree with last-write order.
                for (letter, seconds) in [
                    ("a", 30),
                    ("b", 10),
                    ("c", 20),
                    ("d", 1),
                    ("e", 2),
                    ("f", 3),
                ] {
                    let owner = home.join(letter);
                    std::fs::create_dir(&owner)?;
                    let path = cache(home, &letter.repeat(64), &owner)?;
                    std::fs::create_dir(path.join("nested"))?;
                    std::fs::write(path.join("nested/artifact"), b"build")?;
                    // Nested writes, rather than root mtimes, determine order.
                    for item in [
                        path.clone(),
                        path.join(CARGO_CACHE_OWNER),
                        path.join("artifact"),
                        path.join("nested"),
                    ] {
                        std::fs::File::open(item)?.set_modified(std::time::UNIX_EPOCH)?;
                    }
                    std::fs::File::open(path.join("nested/artifact"))?.set_modified(
                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
                    )?;
                    paths.push(path);
                    owners.push(owner);
                }
                let policy = CacheGcPolicy {
                    free_floor_bytes: Some(floor),
                    dry_run,
                    project_owners: owners[..5].iter().cloned().collect(),
                    ..CacheGcPolicy::default()
                };
                let mut observations = [0, 0, 50, 100].into_iter();
                let report = gc_caches_with_free_space(
                    home,
                    &policy,
                    |owner| {
                        if owner == owners[3] {
                            return Err(WorkerState::Live);
                        }
                        if owner == owners[4] {
                            return Err(WorkerState::Unknown);
                        }
                        locks
                            .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                            .map_err(|_| WorkerState::Unknown)
                    },
                    |_| Ok(observations.next().unwrap_or(100)),
                )?;
                let expected = if floor == u64::MAX {
                    vec![paths[1].clone(), paths[2].clone(), paths[0].clone()]
                } else if dry_run {
                    vec![paths[1].clone()]
                } else {
                    vec![paths[1].clone(), paths[2].clone()]
                };
                assert_eq!(
                    report
                        .removed
                        .iter()
                        .map(|entry| entry.path.clone())
                        .collect::<Vec<_>>(),
                    expected
                );
                assert!(
                    report
                        .removed
                        .iter()
                        .all(|entry| entry.reason == CacheGcReason::FreeFloor)
                );
                assert!(
                    report
                        .kept
                        .iter()
                        .any(|entry| entry.path == paths[3] && entry.reason == CacheGcReason::Live)
                );
                assert!(
                    report.kept.iter().any(|entry| entry.path == paths[4]
                        && entry.reason == CacheGcReason::Unobservable)
                );
                assert_eq!(report.free_bytes_before, Some(0));
                if floor != u64::MAX {
                    assert!(report.free_bytes_after.is_some_and(|value| value >= floor));
                } else {
                    assert!(report.free_bytes_after.is_some_and(|value| value < floor));
                }
                assert_eq!(report.free_bytes_estimated, dry_run);
                assert_eq!(paths[0].is_dir(), dry_run || floor != u64::MAX);
                assert!(paths[3..].iter().all(|path| path.is_dir()));
                assert_eq!(paths[1].is_dir(), dry_run);
            }
            Ok(())
        }

        #[test]
        fn free_floor_rechecks_writes_and_owner_before_detaching()
        -> Result<(), Box<dyn std::error::Error>> {
            for change_owner in [false, true] {
                let temporary = tempfile::tempdir()?;
                let home = temporary.path();
                let owner = home.join("owner");
                std::fs::create_dir(&owner)?;
                let path = cache(home, &"a".repeat(64), &owner)?;
                let locks = test_writer_lock(home)?;
                let policy = CacheGcPolicy {
                    free_floor_bytes: Some(1),
                    project_owners: [owner].into(),
                    ..CacheGcPolicy::default()
                };
                let report = gc_caches_with_free_space(
                    home,
                    &policy,
                    |_| {
                        // A writer changed the cache after candidate measurement.
                        let result = if change_owner {
                            std::fs::write(path.join(CARGO_CACHE_OWNER), b"changed owner")
                        } else {
                            std::fs::File::open(path.join("artifact")).and_then(|file| {
                                file.set_modified(
                                    std::time::SystemTime::now()
                                        + std::time::Duration::from_secs(10),
                                )
                            })
                        };
                        result.map_err(|_| WorkerState::Unknown)?;
                        locks
                            .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                            .map_err(|_| WorkerState::Unknown)
                    },
                    |_| Ok(0),
                )?;
                assert!(report.removed.is_empty());
                assert!(path.join("artifact").is_file());
            }
            for home in [Path::new("relative"), Path::new("/some/../home")] {
                assert!(gc_worktree_cargo_caches(home, &CacheGcPolicy::default()).is_err());
            }
            Ok(())
        }

        #[test]
        fn gc_tombstone_retry_completes_after_interrupted_sweep()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let path = cache(home, &"a".repeat(64), &home.join("missing"))?;
            let mut marker = CARGO_CACHE_OWNER_PREFIX.to_vec();
            marker.extend_from_slice(&1_u64.to_le_bytes());
            marker.extend_from_slice(&2_u64.to_le_bytes());
            marker.extend_from_slice(home.join("missing").as_os_str().as_encoded_bytes());
            std::fs::write(path.join(CARGO_CACHE_OWNER), marker)?;
            let marker = std::fs::read(path.join(CARGO_CACHE_OWNER))?;
            let error = sweep(
                home,
                &CacheGcPolicy::default(),
                Arc::new(|event, path| {
                    if event == "unlink" && path.file_name() == Some(OsStr::new("artifact")) {
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    Ok(())
                }),
            )
            .expect_err("injected unlink failure leaves a tombstone");
            assert!(error.report.removed.is_empty());
            let root = path.parent().ok_or("cache parent")?;
            let tombstone = std::fs::read_dir(root)?.next().ok_or("tombstone")??.path();
            let fingerprint: String = Sha256::digest(&marker)[..16]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert!(
                tombstone
                    .file_name()
                    .ok_or("name")?
                    .to_string_lossy()
                    .contains(&fingerprint)
            );
            assert_eq!(std::fs::read(tombstone.join(CARGO_CACHE_OWNER))?, marker);
            let report = gc_worktree_cargo_caches(home, &CacheGcPolicy::default())?;
            assert_eq!(report.removed.len(), 1);
            assert_eq!(report.removed[0].reason, CacheGcReason::Tombstone);
            assert!(!tombstone.exists());
            Ok(())
        }

        #[test]
        fn gc_continues_after_root_readdir_error() -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let first = cache(home, &"a".repeat(64), &home.join("missing-a"))?;
            let unreadable = cache(home, &"b".repeat(64), &home.join("missing-b"))?;
            let last = cache(home, &"c".repeat(64), &home.join("missing-c"))?;
            let blocked = unreadable.clone();
            let error = sweep(
                home,
                &CacheGcPolicy::default(),
                Arc::new(move |event, path| {
                    if event == "readdir" && path == blocked {
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    Ok(())
                }),
            )
            .expect_err("root entry read failure is reported after sweeping readable caches");
            assert!(!first.exists());
            assert!(!last.exists());
            assert!(unreadable.is_dir());
            assert_eq!(error.report.removed.len(), 2);
            let message = error.to_string();
            assert!(message.contains("readdir"), "{message}");
            assert!(message.contains("PermissionDenied"), "{message}");
            Ok(())
        }

        #[test]
        fn lane_cache_gc_removes_large_cargo_registry_tree()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let path = cache(home, &"a".repeat(64), &home.join("missing-worktree"))?;
            let outside = home.join("outside");
            std::fs::create_dir(&outside)?;
            std::fs::write(outside.join("keep"), b"must survive")?;
            std::os::unix::fs::symlink(&outside, path.join("outside.lock"))?;
            std::fs::create_dir(path.join("directory.lock"))?;
            let registry = path.join("cargo-home/registry/src/index.crates.io-example");
            let target = path.join("target/debug");
            std::fs::create_dir_all(&target)?;
            for index in 0..2048 {
                std::fs::write(
                    target.join(format!("artifact-{index:04}")),
                    b"build artifact",
                )?;
            }
            for index in 0..72 {
                let source = registry.join(format!("crate-{index:02}/src"));
                std::fs::create_dir_all(&source)?;
                std::fs::write(
                    source.parent().ok_or("crate root")?.join("Cargo.lock"),
                    b"# lockfile",
                )?;
                for file in 0..32 {
                    std::fs::write(source.join(format!("module-{file:02}.rs")), b"// source")?;
                }
            }

            // Exercise the public GC entry point, including its root snapshot
            // and descriptor-relative payload removal.
            let result = gc_worktree_cargo_caches(home, &CacheGcPolicy::default());
            assert!(result.is_ok(), "large cache removal failed: {result:#?}");
            assert_eq!(result?.removed.len(), 1);
            assert_eq!(
                std::fs::read_dir(path.parent().ok_or("cache parent")?)?.count(),
                0
            );
            assert_eq!(std::fs::read(outside.join("keep"))?, b"must survive");
            Ok(())
        }

        #[test]
        fn gc_detaches_before_deleting_and_keeps_a_new_lane_cache()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let path = cache(home, &"a".repeat(64), &home.join("missing"))?;
            let report = sweep(
                home,
                &CacheGcPolicy::default(),
                Arc::new(|event, path| {
                    if event == "after rename" {
                        std::fs::create_dir(path)?;
                        std::fs::write(path.join("fresh"), b"new lane")?;
                    }
                    Ok(())
                }),
            )?;
            assert_eq!(report.removed.len(), 1);
            assert_eq!(std::fs::read(path.join("fresh"))?, b"new lane");
            assert!(!path.join("artifact").exists());
            Ok(())
        }

        #[test]
        fn gc_reports_protected_owner_without_probing_writer()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let owner = home.join("owner");
            std::fs::create_dir(&owner)?;
            let path = cache(home, &"a".repeat(64), &owner)?;
            for (state, reason) in [
                (WriterState::Live, CacheGcReason::Live),
                (WriterState::Unknown, CacheGcReason::Unobservable),
            ] {
                for dry_run in [true, false] {
                    let policy = CacheGcPolicy {
                        idle_before_ns: Some(i128::MAX),
                        dry_run,
                        project_owners: [owner.clone()].into(),
                        protected_owners: [(owner.clone(), state)].into(),
                        selected_only: false,
                        free_floor_bytes: None,
                    };
                    let mut probes = 0;
                    let report = gc_worktree_cargo_caches_with_writer_guard(home, &policy, |_| {
                        probes += 1;
                        Err(WorkerState::Unknown)
                    })?;
                    assert_eq!(probes, 0, "the listing already protects this owner");
                    assert!(report.removed.is_empty());
                    assert_eq!(report.kept.len(), 1);
                    assert_eq!(report.kept[0].reason, reason);
                    assert!(path.join("artifact").is_file());
                }
            }
            Ok(())
        }

        #[test]
        fn gc_idle_refuses_writers_that_became_live_or_unobservable()
        -> Result<(), Box<dyn std::error::Error>> {
            for live in [true, false] {
                let temporary = tempfile::tempdir()?;
                let home = temporary.path();
                let owner = home.join("owner");
                std::fs::create_dir(&owner)?;
                let path = cache(home, &"a".repeat(64), &owner)?;
                let locks = test_writer_lock(home)?;
                let held = if live {
                    Some(locks.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)?)
                } else {
                    std::fs::remove_file(home.join("writer-state/writer.lock"))?;
                    None
                };
                // The listing observed a free writer; the lock changed later.
                let policy = CacheGcPolicy {
                    idle_before_ns: Some(i128::MAX),
                    project_owners: [owner].into(),
                    ..CacheGcPolicy::default()
                };
                let report = gc_worktree_cargo_caches_with_writer_guard(home, &policy, |_| {
                    locks
                        .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                        .map_err(|error| match error {
                            FsError::LockTimeout { .. } => WorkerState::Live,
                            _ => WorkerState::Unknown,
                        })
                })?;
                assert!(report.removed.is_empty());
                assert_eq!(report.kept.len(), 1);
                assert_eq!(
                    report.kept[0].reason,
                    if live {
                        CacheGcReason::Live
                    } else {
                        CacheGcReason::Unobservable
                    }
                );
                assert!(path.join("artifact").is_file());
                assert_eq!(home.join("writer-state/writer.lock").exists(), live);
                drop(held);
            }
            Ok(())
        }

        #[test]
        fn gc_idle_releases_writer_lock_before_tombstone_cleanup_and_writer_retries()
        -> Result<(), Box<dyn std::error::Error>> {
            use provenant_core::{AttemptId, GitOid, RunId};
            use provenant_os::clock::SystemClock;
            use provenant_os::fs::Directory;
            use provenant_supervisor::custody::{self, CustodyError, Scope, WriterBinding};
            use std::sync::atomic::{AtomicUsize, Ordering};

            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let owner = home.join("owner");
            std::fs::create_dir(&owner)?;
            let path = cache(home, &"a".repeat(64), &owner)?;
            let locks = Arc::new(test_writer_lock(home)?);
            let observed_locks = locks.clone();
            let renames = Arc::new(AtomicUsize::new(0));
            let observed_renames = renames.clone();
            let retried = Arc::new(AtomicUsize::new(0));
            let observed_retried = retried.clone();
            let binding = WriterBinding::from_verified(
                &Directory::open_anchor(&owner)?,
                &Directory::open_anchor(&owner)?,
                "refs/heads/test",
                GitOid::parse(&"a".repeat(40))?,
            )?;
            let writer_home = home.to_path_buf();
            let writer_owner = owner.clone();
            let policy = CacheGcPolicy {
                idle_before_ns: Some(i128::MAX),
                project_owners: [owner].into(),
                ..CacheGcPolicy::default()
            };
            let report = sweep_with_writer_guard(
                home,
                &policy,
                Arc::new(move |event, _| {
                    let launch = || {
                        let clock = SystemClock;
                        custody::acquire_writer(
                            Directory::open_anchor(&writer_home.join("writer-state"))?,
                            Scope {
                                run: RunId::from_bytes([1; 16]),
                                attempt: AttemptId::from_bytes([2; 16]),
                                generation: 1,
                            },
                            binding.clone(),
                            WaitBudget::nonblocking(clock.mono_now()),
                            &CancelToken::new(),
                            &clock,
                        )
                    };
                    if matches!(event, "before rename" | "after rename") {
                        assert!(matches!(
                            observed_locks
                                .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive,),
                            Err(FsError::LockTimeout { .. })
                        ));
                        assert!(matches!(launch(), Err(CustodyError::Busy)));
                        observed_renames.fetch_add(1, Ordering::SeqCst);
                    } else if observed_renames.load(Ordering::SeqCst) == 2
                        && observed_retried.fetch_add(1, Ordering::SeqCst) == 0
                    {
                        // The first filesystem event after rename_noreplace
                        // returns must allow the refused writer to retry.
                        let _writer = launch().map_err(io::Error::other)?;
                        let fresh = cache(&writer_home, &"a".repeat(64), &writer_owner)?;
                        std::fs::write(fresh.join("artifact"), b"fresh writer output")?;
                    }
                    Ok(())
                }),
                &mut |_| {
                    locks
                        .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                        .map_err(|_| WorkerState::Unknown)
                },
            )?;
            assert_eq!(renames.load(Ordering::SeqCst), 2);
            assert_eq!(report.removed.len(), 1);
            assert_eq!(report.removed[0].reason, CacheGcReason::Idle);
            assert!(retried.load(Ordering::SeqCst) > 0);
            assert_eq!(
                std::fs::read(path.join("artifact"))?,
                b"fresh writer output"
            );
            assert!(path.join(CARGO_CACHE_OWNER).is_file());
            let _released =
                locks.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)?;
            Ok(())
        }

        #[test]
        fn gc_idle_dry_run_never_holds_probe_during_measurement()
        -> Result<(), Box<dyn std::error::Error>> {
            use std::sync::atomic::{AtomicBool, Ordering};

            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let owner = home.join("owner");
            std::fs::create_dir(&owner)?;
            let path = cache(home, &"a".repeat(64), &owner)?;
            let locks = Arc::new(test_writer_lock(home)?);
            let observed_locks = locks.clone();
            let probed = Arc::new(AtomicBool::new(false));
            let policy = CacheGcPolicy {
                idle_before_ns: Some(i128::MAX),
                dry_run: true,
                project_owners: [owner].into(),
                ..CacheGcPolicy::default()
            };
            let report = sweep_with_writer_guard(
                home,
                &policy,
                Arc::new(move |event, _| {
                    if event == "readdir" {
                        let _writer = observed_locks
                            .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                            .map_err(io::Error::other)?;
                    }
                    Ok(())
                }),
                &mut |_| {
                    probed.store(true, Ordering::SeqCst);
                    locks
                        .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                        .map_err(|_| WorkerState::Unknown)
                },
            )?;
            assert!(probed.load(Ordering::SeqCst));
            assert_eq!(report.removed.len(), 1);
            assert!(path.join("artifact").is_file());
            Ok(())
        }

        #[test]
        fn gc_rechecks_orphan_and_idle_conditions_before_detaching()
        -> Result<(), Box<dyn std::error::Error>> {
            use std::sync::atomic::{AtomicUsize, Ordering};
            for idle in [false, true] {
                let temporary = tempfile::tempdir()?;
                let home = temporary.path();
                let owner = home.join("owner");
                if idle {
                    std::fs::create_dir(&owner)?;
                }
                let path = cache(home, &"a".repeat(64), &owner)?;
                for file in [
                    path.join(CARGO_CACHE_OWNER),
                    path.join("artifact"),
                    path.clone(),
                ] {
                    std::fs::File::open(file)?.set_modified(std::time::UNIX_EPOCH)?;
                }
                let policy = CacheGcPolicy {
                    idle_before_ns: Some(1),
                    project_owners: [owner.clone()].into(),
                    ..CacheGcPolicy::default()
                };
                let locks = test_writer_lock(home)?;
                let reads = AtomicUsize::new(0);
                let report = sweep_with_writer_guard(
                    home,
                    &policy,
                    Arc::new(move |event, marker| {
                        if event == "read" && reads.fetch_add(1, Ordering::SeqCst) == 1 {
                            if idle {
                                std::fs::write(
                                    marker.with_file_name("artifact"),
                                    b"launching lane",
                                )?;
                            } else {
                                std::fs::create_dir(&owner)?;
                            }
                        }
                        Ok(())
                    }),
                    &mut |_| {
                        locks
                            .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                            .map_err(|_| WorkerState::Unknown)
                    },
                )?;
                assert!(report.removed.is_empty());
                assert!(path.join(CARGO_CACHE_OWNER).is_file());
            }
            Ok(())
        }

        #[test]
        fn gc_refuses_replacement_identity_after_rename() -> Result<(), Box<dyn std::error::Error>>
        {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let path = cache(home, &"a".repeat(64), &home.join("missing"))?;
            let error = sweep(
                home,
                &CacheGcPolicy::default(),
                Arc::new(|event, path| {
                    if event == "before rename"
                        && path
                            .file_name()
                            .and_then(OsStr::to_str)
                            .is_some_and(is_cache_digest)
                    {
                        std::fs::rename(path, path.with_file_name("saved"))?;
                        std::fs::create_dir(path)?;
                        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
                        std::fs::write(path.join("replacement"), b"must survive")?;
                    }
                    Ok(())
                }),
            )
            .expect_err("replacement identity must be refused");
            assert_eq!(error.report.bytes_freed, 0);
            assert!(path.with_file_name("saved").join("artifact").is_file());
            let tombstone = std::fs::read_dir(path.parent().ok_or("cache parent")?)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .find(|entry| entry.file_name().to_string_lossy().starts_with(".gc-"))
                .ok_or("tombstone")?;
            assert!(tombstone.path().join("replacement").is_file());
            assert!(
                gc_worktree_cargo_caches(home, &CacheGcPolicy::default())?
                    .removed
                    .is_empty()
            );
            assert!(tombstone.path().join("replacement").is_file());
            Ok(())
        }

        #[test]
        fn gc_reports_swap_during_verified_open() -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            cache(home, &"a".repeat(64), &home.join("missing"))?;
            let error = sweep(
                home,
                &CacheGcPolicy::default(),
                Arc::new(|event, path| {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if event == "open verified" && name.starts_with(".gc-") {
                        return Err(io::Error::other("swapped"));
                    }
                    Ok(())
                }),
            )
            .expect_err("swapped tombstone must be refused");
            let message = error.to_string();
            assert!(
                message.contains("lane cache open verified directory failed at \".gc-"),
                "{message}"
            );
            assert!(message.contains("entry changed identity"), "{message}");
            assert_eq!(error.report.bytes_freed, 0);
            Ok(())
        }

        #[test]
        fn gc_quarantines_changed_owner_after_rename() -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let path = cache(home, &"a".repeat(64), &home.join("missing"))?;
            let root = path.parent().ok_or("cache parent")?.to_path_buf();
            let foreign = home.join("foreign");
            std::fs::create_dir(&foreign)?;
            let foreign_cache = cache(home, "foreign-marker", &foreign)?;
            let marker = std::fs::read(foreign_cache.join(CARGO_CACHE_OWNER))?;
            let scan_root = root.clone();
            let error = sweep(
                home,
                &CacheGcPolicy::default(),
                Arc::new(move |event, path| {
                    if event == "after rename"
                        && path
                            .file_name()
                            .and_then(OsStr::to_str)
                            .is_some_and(is_cache_digest)
                    {
                        for entry in std::fs::read_dir(&scan_root)? {
                            let entry = entry?;
                            if entry.file_name().to_string_lossy().starts_with(".gc-") {
                                std::fs::write(entry.path().join(CARGO_CACHE_OWNER), &marker)?;
                            }
                        }
                    }
                    Ok(())
                }),
            )
            .expect_err("changed owner must be refused");
            assert_eq!(error.report.bytes_freed, 0);
            assert!(
                gc_worktree_cargo_caches(home, &CacheGcPolicy::default())?
                    .removed
                    .is_empty()
            );
            let rejected = std::fs::read_dir(root)?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".gc-refused-")
                })
                .ok_or("quarantine")?;
            assert!(rejected.path().join("artifact").is_file());
            Ok(())
        }

        #[test]
        fn gc_failure_keeps_marker_reports_progress_and_retries_tombstone()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let path = cache(home, &"a".repeat(64), &home.join("missing"))?;
            std::fs::write(path.join("z-fail"), b"cannot remove yet")?;
            let artifact = std::fs::metadata(path.join("artifact"))?;
            let bytes = artifact.blocks() * 512;
            let ordinary = cache(home, &"b".repeat(64), &home.join("also-missing"))?;
            let error = sweep(
                home,
                &CacheGcPolicy::default(),
                Arc::new(|event, path| {
                    if event == "unlink" && path.file_name() == Some(OsStr::new("z-fail")) {
                        assert!(path.with_file_name(CARGO_CACHE_OWNER).is_file());
                        return Err(io::Error::from_raw_os_error(13));
                    }
                    Ok(())
                }),
            )
            .expect_err("injected unlink failure");
            let message = error.to_string();
            assert!(message.contains("unlink"), "{message}");
            assert!(message.contains("/z-fail"), "{message}");
            assert!(message.contains("PermissionDenied"), "{message}");
            assert!(message.contains("os error 13"), "{message}");
            assert!(
                message.contains("lane cache unlink failed at \".gc-"),
                "{message}"
            );
            assert_eq!(error.report.removed.len(), 1);
            assert!(!ordinary.exists());
            let root = path.parent().ok_or("cache parent")?;
            let tombstone = std::fs::read_dir(root)?.next().ok_or("tombstone")??.path();
            assert!(tombstone.join(CARGO_CACHE_OWNER).is_file());
            assert!(tombstone.join("z-fail").is_file());
            // Filesystem order determines whether the artifact preceded the
            // failed unlink. Account for exactly the removals that completed.
            let payload_bytes = if tombstone.join("artifact").exists() {
                0
            } else {
                bytes
            };
            assert_eq!(
                error.report.bytes_freed,
                payload_bytes + error.report.removed[0].bytes
            );
            let report = gc_worktree_cargo_caches(home, &CacheGcPolicy::default())?;
            assert_eq!(report.removed.len(), 1);
            assert_eq!(report.removed[0].reason, CacheGcReason::Tombstone);
            assert!(!tombstone.exists());
            Ok(())
        }

        #[test]
        fn gc_retry_refuses_unverified_tombstone_when_quarantine_failed()
        -> Result<(), Box<dyn std::error::Error>> {
            for change_owner in [false, true] {
                let temporary = tempfile::tempdir()?;
                let home = temporary.path();
                let path = cache(home, &"a".repeat(64), &home.join("missing"))?;
                let root = path.parent().ok_or("cache parent")?.to_path_buf();
                let foreign = home.join("foreign");
                std::fs::create_dir(&foreign)?;
                let foreign_cache = cache(home, "foreign-marker", &foreign)?;
                let marker = std::fs::read(foreign_cache.join(CARGO_CACHE_OWNER))?;
                let scan_root = root.clone();
                let error = sweep(
                    home,
                    &CacheGcPolicy::default(),
                    Arc::new(move |event, path| {
                        let original = path
                            .file_name()
                            .and_then(OsStr::to_str)
                            .is_some_and(is_cache_digest);
                        if event == "before rename" {
                            if !original {
                                return Err(io::ErrorKind::PermissionDenied.into());
                            }
                            if !change_owner {
                                std::fs::rename(path, path.with_file_name("saved"))?;
                                std::fs::create_dir(path)?;
                                std::fs::set_permissions(
                                    path,
                                    std::fs::Permissions::from_mode(0o700),
                                )?;
                                std::fs::write(path.join("artifact"), b"replacement")?;
                            }
                        }
                        if event == "after rename" && change_owner {
                            for entry in std::fs::read_dir(&scan_root)? {
                                let entry = entry?;
                                if entry.file_name().to_string_lossy().starts_with(".gc-") {
                                    std::fs::write(entry.path().join(CARGO_CACHE_OWNER), &marker)?;
                                }
                            }
                        }
                        Ok(())
                    }),
                )
                .expect_err("quarantine fails after verification refuses replacement");
                assert_eq!(error.report.bytes_freed, 0);
                let tombstone = std::fs::read_dir(root)?
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .find(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .and_then(cache_tombstone)
                            .is_some()
                    })
                    .ok_or("unverified tombstone")?;
                let retry = gc_worktree_cargo_caches(home, &CacheGcPolicy::default())
                    .expect_err("retry must still refuse the replacement");
                assert_eq!(retry.report.bytes_freed, 0);
                assert!(retry.report.removed.is_empty());
                assert!(tombstone.path().join("artifact").is_file());
            }
            Ok(())
        }

        #[test]
        fn gc_preserves_name_mode_and_symlink_protections() -> Result<(), Box<dyn std::error::Error>>
        {
            use std::os::unix::fs::symlink;

            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let missing = home.join("missing");
            let outside = home.join("outside");
            std::fs::create_dir(&outside)?;
            std::fs::write(outside.join("precious"), b"keep")?;
            let invalid_name = cache(home, "not-a-cache", &missing)?;
            let unsafe_mode = cache(home, &"a".repeat(64), &missing)?;
            std::fs::set_permissions(&unsafe_mode, std::fs::Permissions::from_mode(0o755))?;
            let linked_marker = cache(home, &"b".repeat(64), &missing)?;
            std::fs::remove_file(linked_marker.join(CARGO_CACHE_OWNER))?;
            symlink(
                outside.join("precious"),
                linked_marker.join(CARGO_CACHE_OWNER),
            )?;
            let collectible = cache(home, &"c".repeat(64), &missing)?;
            symlink(&outside, collectible.join("outside-link"))?;
            let root = collectible.parent().ok_or("cache parent")?;
            let linked_cache = root.join("d".repeat(64));
            symlink(&outside, &linked_cache)?;
            let linked_tombstone = root.join(format!(".gc-{}-123", "e".repeat(64)));
            symlink(&outside, &linked_tombstone)?;
            let invalid_tombstone = cache(home, ".gc-invalid-123", &missing)?;
            let report = gc_worktree_cargo_caches(home, &CacheGcPolicy::default())?;
            assert_eq!(report.removed.len(), 1);
            assert!(!collectible.exists());
            for retained in [
                invalid_name,
                unsafe_mode,
                linked_marker,
                linked_cache,
                linked_tombstone,
                invalid_tombstone,
            ] {
                assert!(retained.exists());
            }
            assert_eq!(std::fs::read(outside.join("precious"))?, b"keep");
            Ok(())
        }

        #[test]
        fn gc_retries_tombstone_after_final_rmdir_failure() -> Result<(), Box<dyn std::error::Error>>
        {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let path = cache(home, &"a".repeat(64), &home.join("missing"))?;
            let error = sweep(
                home,
                &CacheGcPolicy::default(),
                Arc::new(|event, path| {
                    if event == "rmdir"
                        && path
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with(".gc-"))
                    {
                        assert!(!path.join(CARGO_CACHE_OWNER).exists());
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    Ok(())
                }),
            )
            .expect_err("injected rmdir failure");
            assert!(error.report.removed.is_empty());
            assert!(error.report.bytes_freed > 0);
            // A held writer can leave content after the marker was deleted.
            let tombstone = std::fs::read_dir(path.parent().ok_or("cache parent")?)?
                .next()
                .ok_or("tombstone")??
                .path();
            std::fs::write(tombstone.join("late-write"), b"writer remnant")?;
            let report = gc_worktree_cargo_caches(home, &CacheGcPolicy::default())?;
            assert_eq!(report.removed.len(), 1);
            assert!(report.bytes_freed > 0);
            assert_eq!(
                std::fs::read_dir(path.parent().ok_or("cache parent")?)?.count(),
                0
            );
            Ok(())
        }
    }

    #[test]
    fn lane_cache_gc_keeps_existing_foreign_owners_even_with_old_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir()?;
        let home = temporary.path();
        let root = home.join("Library/Caches/provenant-lane-cargo");
        std::fs::create_dir_all(&root)?;
        let owner = home.join("other-project");
        std::fs::create_dir(&owner)?;
        let identity = std::fs::metadata(&owner)?;
        for (letter, inode) in [("a", identity.ino()), ("b", identity.ino() + 1)] {
            let cache = root.join(letter.repeat(64));
            std::fs::create_dir(&cache)?;
            std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))?;
            let mut marker = CARGO_CACHE_OWNER_PREFIX.to_vec();
            marker.extend_from_slice(&identity.dev().to_le_bytes());
            marker.extend_from_slice(&inode.to_le_bytes());
            marker.extend_from_slice(owner.as_os_str().as_encoded_bytes());
            std::fs::write(cache.join(CARGO_CACHE_OWNER), marker)?;
        }
        let policy = CacheGcPolicy {
            idle_before_ns: Some(i128::MAX),
            ..CacheGcPolicy::default()
        };
        let report = gc_worktree_cargo_caches(home, &policy)?;
        assert!(
            report.removed.is_empty(),
            "existing foreign owners must be kept"
        );
        assert_eq!(report.kept.len(), 2);
        assert!(
            report
                .kept
                .iter()
                .all(|entry| entry.reason == CacheGcReason::Foreign)
        );
        assert!(root.join("a".repeat(64)).is_dir());
        assert!(root.join("b".repeat(64)).is_dir());
        Ok(())
    }

    #[test]
    fn lane_cache_gc_removes_orphaned_and_idle_and_spares_live_and_recent()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir()?;
        let home = temporary.path();
        let cache_root = home.join("Library/Caches/provenant-lane-cargo");
        std::fs::create_dir_all(&cache_root)?;
        let mut caches = Vec::new();
        for (letter, name) in [("a", "gone"), ("b", "idle"), ("c", "live")] {
            let cache = cache_root.join(letter.repeat(64));
            std::fs::create_dir(&cache)?;
            std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))?;
            std::fs::write(cache.join("artifact"), b"0123456789")?;
            let owner = home.join(name);
            std::fs::create_dir(&owner)?;
            let identity = std::fs::metadata(&owner)?;
            let mut marker = CARGO_CACHE_OWNER_PREFIX.to_vec();
            marker.extend_from_slice(&identity.dev().to_le_bytes());
            marker.extend_from_slice(&identity.ino().to_le_bytes());
            marker.extend_from_slice(owner.as_os_str().as_encoded_bytes());
            std::fs::write(cache.join(CARGO_CACHE_OWNER), marker)?;
            caches.push((cache, owner));
        }
        std::fs::remove_dir(&caches[0].1)?;
        let locks = test_writer_lock(home)?;
        let mut writer_guard = |_: &Path| {
            locks
                .try_lock_existing(
                    OsStr::new("writer.lock"),
                    provenant_os::fs::LockMode::Exclusive,
                )
                .map_err(|_| WorkerState::Unknown)
        };
        let mut policy = CacheGcPolicy {
            idle_before_ns: Some(i128::MAX),
            dry_run: true,
            project_owners: caches.iter().map(|(_, owner)| owner.clone()).collect(),
            protected_owners: [(caches[2].1.clone(), WriterState::Live)].into(),
            free_floor_bytes: None,
            selected_only: false,
        };

        policy.selected_only = true;
        policy.project_owners = [caches[1].1.clone()].into();
        let scoped = gc_worktree_cargo_caches_with_writer_guard(home, &policy, &mut writer_guard)?;
        assert_eq!(scoped.removed.len(), 1);
        assert_eq!(scoped.removed[0].path, caches[1].0);
        policy.selected_only = false;
        policy.project_owners = caches.iter().map(|(_, owner)| owner.clone()).collect();

        let dry = gc_worktree_cargo_caches_with_writer_guard(home, &policy, &mut writer_guard)?;
        assert_eq!(dry.removed.len(), 2);
        assert!(caches.iter().all(|(cache, _)| cache.is_dir()));

        policy.dry_run = false;
        let report = gc_worktree_cargo_caches_with_writer_guard(home, &policy, &mut writer_guard)?;
        let reasons: Vec<_> = report.removed.iter().map(|e| e.reason).collect();
        assert!(reasons.contains(&CacheGcReason::Orphaned));
        assert!(reasons.contains(&CacheGcReason::Idle));
        assert!(report.bytes_freed >= 20);
        assert!(!caches[0].0.exists());
        assert!(!caches[1].0.exists());
        assert!(caches[2].0.is_dir(), "protected owner keeps its cache");

        // A recent cache is kept when the threshold predates its writes.
        policy.idle_before_ns = Some(0);
        policy.protected_owners.clear();
        assert!(
            gc_worktree_cargo_caches_with_writer_guard(home, &policy, &mut writer_guard)?
                .removed
                .is_empty()
        );
        assert!(caches[2].0.is_dir());
        Ok(())
    }
}
