//! Lane cache collection.

use crate::permissions::{PermissionError, validate_absolute};
use crate::worktrees::Occupancy;
use provenant_config::RetentionIssue;
use provenant_os::fs::{
    Directory, DirectoryPosition, Entries, FileKind, ForeignLock, FsError, LockGuard, LockMode,
    Metadata, PayloadTree,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub(crate) const CARGO_CACHE_OWNER: &str = ".provenant-worktree-owner";
pub(crate) const CARGO_CACHE_OWNER_PREFIX: &[u8] = b"provenant-lane-cargo-owner-v2\n";
pub(crate) const CARGO_CACHE_OWNER_VERSION: &[u8] = match CARGO_CACHE_OWNER_PREFIX.split_last() {
    Some((_, version)) => version,
    None => CARGO_CACHE_OWNER_PREFIX,
};
const MAX_CACHE_SWEEP_DEPTH: usize = 32;
const MAX_TREE_MEASURE_ENTRIES: usize = 4096;
const MAX_TREE_CURSOR_BYTES: usize = 14 * 1024;
const MAX_CACHE_CURSOR_BYTES: usize = 14 * 1024;
const CACHE_DELETE_BATCH_ENTRIES: usize = 64;

/// Why a lane cache was removed, would be removed or was kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheGcReason {
    /// Writer flock contention, without evidence of a live process.
    Busy,
    /// The recorded owner path no longer exists.
    Orphaned,
    /// Nothing under the cache was written since the idle threshold.
    Idle,
    /// Additional removal to reach the requested available disk space.
    FreeFloor,
    /// Additional removal to reach the configured total cache bound.
    TotalCap,
    /// A detached cache left by an interrupted sweep.
    Tombstone,
    /// An existing owner outside the selected project; always kept.
    Foreign,
    /// A process was observed live in the owner tree; always kept.
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
    /// Total cache byte bound, including protected caches.
    pub max_total_bytes: Option<u64>,
    /// Report without deleting.
    pub dry_run: bool,
    /// Canonical worktrees listed by the selected project.
    pub project_owners: BTreeSet<PathBuf>,
    /// Limit collection to listed owners, including orphan and detached output.
    pub selected_only: bool,
    /// Listing observations for canonical owners with live or unknown writers;
    /// never removed or re-probed during the sweep.
    pub protected_owners: BTreeMap<PathBuf, Occupancy>,
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
    pub errors: Vec<RetentionIssue>,
    /// Preview measurements stopped at the shared entry or time bound.
    pub bytes_estimated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes_after: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_bytes_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_bytes_after: Option<u64>,
    /// Dry-run free space predictions include estimated reclaimable bytes.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub free_bytes_estimated: bool,
    /// Candidate path at which this sweep stopped and can resume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<PathBuf>,
    /// Owner-specific continuation for the interrupted candidate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<String>,
}

/// A failed sweep still reports completed removals and bytes already freed.
#[derive(Debug, Error)]
#[error("{source}")]
pub struct CacheGcError {
    pub report: Box<CacheGcReport>,
    #[source]
    pub source: Box<PermissionError>,
}

/// Controls one resumable lane-cache sweep. A missing callback permits an
/// unbounded one-off operation; the host GC supplies its expiring deadline.
pub struct CacheGcScan<'a> {
    pub start_at: Option<&'a Path>,
    pub detail: Option<&'a str>,
    pub keep_going: Option<&'a dyn Fn() -> bool>,
}

impl Default for CacheGcScan<'_> {
    fn default() -> Self {
        Self {
            start_at: None,
            detail: None,
            keep_going: None,
        }
    }
}

impl CacheGcScan<'_> {
    fn permits(&self) -> bool {
        self.keep_going.is_none_or(|keep_going| keep_going())
    }
}

/// Allocated bytes and newest modification time for a fully measured tree.
/// Both remain observations, not a promise about physical volume reclamation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeStats {
    pub allocated_bytes: u64,
    pub newest_modified_ns: i128,
}

/// Result of a bounded no-follow tree measurement. Partial statistics are
/// deliberately omitted so they cannot be mistaken for a complete survey.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeMeasureProgress {
    pub stats: Option<TreeStats>,
    pub interrupted_at: Option<PathBuf>,
    pub continuation: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TreeMeasureCursor {
    stats: TreeStats,
    frames: Vec<TreeMeasureFrameCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TreeMeasureFrameCursor {
    /// `None` is the supplied root; other names are hex-encoded OS bytes.
    name: Option<String>,
    cookie: DirectoryPosition,
    metadata: TreeMeasureMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum CacheGcCursor {
    Root {
        metadata: TreeMeasureMetadata,
        cookie: DirectoryPosition,
    },
    RootSearch {
        metadata: TreeMeasureMetadata,
        cookie: DirectoryPosition,
        target: String,
    },
    Candidate {
        root_metadata: TreeMeasureMetadata,
        cookie: DirectoryPosition,
        current: String,
        next: Option<String>,
        measure: Option<String>,
    },
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TreeMeasureMetadata {
    device: u64,
    inode: u64,
    links: u64,
    size: u64,
    modified_ns: i128,
    changed_ns: i128,
    uid: u32,
    mode: u32,
}

impl From<&Metadata> for TreeMeasureMetadata {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.device,
            inode: metadata.inode,
            links: metadata.links,
            size: metadata.size,
            modified_ns: metadata.modified_ns,
            changed_ns: metadata.changed_ns,
            uid: metadata.uid,
            mode: metadata.mode,
        }
    }
}

impl TreeMeasureMetadata {
    fn matches(self, metadata: &Metadata) -> bool {
        metadata.kind == FileKind::Directory
            && self.device == metadata.device
            && self.inode == metadata.inode
            && self.links == metadata.links
            && self.size == metadata.size
            && self.modified_ns == metadata.modified_ns
            && self.changed_ns == metadata.changed_ns
            && self.uid == metadata.uid
            && self.mode == metadata.mode
    }
}

struct TreeMeasureFrame {
    name: Option<OsString>,
    path: PathBuf,
    directory: Directory,
    entries: Entries,
    metadata: TreeMeasureMetadata,
}

/// Measure an allocated tree without following links. A pass stops after 4,096
/// entries, one second, or the caller's deadline. Its opaque cursor resumes
/// only while every open directory frame retains the same identity and content
/// metadata. A stale cursor restarts at the supplied root.
pub fn measure_tree_with_progress(
    directory: &Directory,
    display_root: &Path,
    detail: Option<&str>,
    keep_going: &dyn Fn() -> bool,
) -> Result<TreeMeasureProgress, PermissionError> {
    let root_metadata = directory
        .metadata()
        .map_err(cache_fs_error("stat measured tree", display_root))?;
    if root_metadata.kind != FileKind::Directory {
        return Err(PermissionError::AnchorMismatch);
    }

    let resumed = detail
        .filter(|detail| detail.len() <= MAX_TREE_CURSOR_BYTES)
        .and_then(|detail| serde_json::from_str::<TreeMeasureCursor>(detail).ok())
        .filter(|cursor| {
            !cursor.frames.is_empty()
                && cursor.frames.len() <= MAX_CACHE_SWEEP_DEPTH
                && cursor.frames[0].name.is_none()
                && cursor.frames[0].metadata.matches(&root_metadata)
        });

    let (mut stats, mut frames) = if let Some(cursor) = resumed {
        match restore_tree_measure_frames(directory, display_root, &cursor.frames) {
            Some(frames) => (cursor.stats, frames),
            None => fresh_tree_measure(directory, display_root, root_metadata)?,
        }
    } else {
        fresh_tree_measure(directory, display_root, root_metadata)?
    };

    let started = std::time::Instant::now();
    let mut visited = 0_usize;
    while let Some(frame) = frames.last_mut() {
        if visited >= MAX_TREE_MEASURE_ENTRIES
            || started.elapsed() >= std::time::Duration::from_secs(1)
            || !keep_going()
        {
            let cursor = tree_measure_cursor(&stats, &mut frames)?;
            let continuation = serde_json::to_string(&cursor).ok();
            return Ok(TreeMeasureProgress {
                stats: None,
                interrupted_at: Some(display_root.to_owned()),
                continuation: continuation.filter(|value| value.len() <= MAX_TREE_CURSOR_BYTES),
            });
        }

        match frame.entries.next() {
            Some(Ok(name)) => {
                let path = frame.path.join(&name);
                let metadata = frame
                    .directory
                    .metadata_at(&name, false)
                    .map_err(cache_fs_error("stat measured entry", &path))?;
                stats.allocated_bytes = stats.allocated_bytes.saturating_add(metadata.allocated);
                stats.newest_modified_ns = stats.newest_modified_ns.max(metadata.modified_ns);
                visited += 1;
                if metadata.kind == FileKind::Directory {
                    if frames.len() >= MAX_CACHE_SWEEP_DEPTH {
                        return Err(PermissionError::CacheSweepDepth);
                    }
                    let child = frame
                        .directory
                        .open_verified_directory(&name)
                        .map_err(cache_fs_error("open measured directory", &path))?;
                    let child_metadata = child
                        .metadata()
                        .map_err(cache_fs_error("stat measured directory", &path))?;
                    if !metadata.unchanged(&child_metadata) {
                        return Err(PermissionError::AnchorMismatch);
                    }
                    let mut entries = child
                        .entries()
                        .map_err(cache_fs_error("opendir measured directory", &path))?;
                    let _ = entries
                        .position()
                        .map_err(cache_fs_error("position measured directory", &path))?;
                    frames.push(TreeMeasureFrame {
                        name: Some(name),
                        path,
                        directory: child,
                        entries,
                        metadata: TreeMeasureMetadata::from(&child_metadata),
                    });
                }
            }
            Some(Err(error)) => {
                return Err(cache_fs_error("readdir measured tree", &frame.path)(error));
            }
            None => {
                frames.pop();
            }
        }
    }

    Ok(TreeMeasureProgress {
        stats: Some(stats),
        ..TreeMeasureProgress::default()
    })
}

fn fresh_tree_measure(
    directory: &Directory,
    display_root: &Path,
    metadata: Metadata,
) -> Result<(TreeStats, Vec<TreeMeasureFrame>), PermissionError> {
    let mut entries = directory
        .entries()
        .map_err(cache_fs_error("opendir measured tree", display_root))?;
    let _ = entries
        .position()
        .map_err(cache_fs_error("position measured tree", display_root))?;
    Ok((
        TreeStats {
            allocated_bytes: metadata.allocated,
            newest_modified_ns: metadata.modified_ns,
        },
        vec![TreeMeasureFrame {
            name: None,
            path: display_root.to_owned(),
            directory: directory
                .try_clone()
                .map_err(cache_fs_error("duplicate measured tree", display_root))?,
            entries,
            metadata: TreeMeasureMetadata::from(&metadata),
        }],
    ))
}

fn restore_tree_measure_frames(
    directory: &Directory,
    display_root: &Path,
    cursors: &[TreeMeasureFrameCursor],
) -> Option<Vec<TreeMeasureFrame>> {
    let root_metadata = directory.metadata().ok()?;
    if !cursors[0].metadata.matches(&root_metadata) {
        return None;
    }
    let mut frames = Vec::with_capacity(cursors.len());
    let mut current = directory.try_clone().ok()?;
    let mut path = display_root.to_owned();
    for (index, cursor) in cursors.iter().enumerate() {
        if index > 0 {
            let name = decode_tree_name(cursor.name.as_deref()?)?;
            path.push(&name);
            current = current.open_verified_directory(&name).ok()?;
        }
        let metadata = current.metadata().ok()?;
        if !cursor.metadata.matches(&metadata) {
            return None;
        }
        let mut entries = current.entries().ok()?;
        entries.seek(cursor.cookie).ok()?;
        frames.push(TreeMeasureFrame {
            name: cursor.name.as_deref().and_then(decode_tree_name),
            path: path.clone(),
            directory: current.try_clone().ok()?,
            entries,
            metadata: cursor.metadata,
        });
    }
    let _ = display_root;
    Some(frames)
}

fn tree_measure_cursor(
    stats: &TreeStats,
    frames: &mut [TreeMeasureFrame],
) -> Result<TreeMeasureCursor, PermissionError> {
    let mut cursors = Vec::with_capacity(frames.len());
    for frame in frames {
        let cookie = frame
            .entries
            .position()
            .map_err(cache_fs_error("position measured tree", &frame.path))?;
        cursors.push(TreeMeasureFrameCursor {
            name: frame.name.as_deref().map(encode_tree_name),
            cookie,
            metadata: frame.metadata,
        });
    }
    Ok(TreeMeasureCursor {
        stats: *stats,
        frames: cursors,
    })
}

fn encode_tree_name(name: &OsStr) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(name.as_bytes().len() * 2);
    for byte in name.as_bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_tree_name(encoded: &str) -> Option<OsString> {
    if encoded.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        bytes.push(digit(pair[0])?.saturating_mul(16) + digit(pair[1])?);
    }
    Some(OsString::from_vec(bytes))
}

/// Remove marked orphan caches and retry detached remnants from earlier sweeps.
pub fn prune_orphaned_worktree_cargo_caches(
    home: &Path,
    scan: &CacheGcScan<'_>,
) -> Result<usize, CacheGcError> {
    gc_worktree_cargo_caches(home, &CacheGcPolicy::default(), scan)
        .map(|report| report.removed.len())
}

/// Remove only the cache derived from a worktree before its removal. The caller
/// holds that worktree's writer lock until this returns. Other orphaned caches
/// and interrupted GC tombstones are left for `provenant gc`.
pub(crate) fn prune_worktree_cargo_cache(
    home: &Path,
    cache: &Path,
) -> Result<u64, PermissionError> {
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
        return Ok(0);
    };
    let mut report = CacheGcReport::default();
    let mut detail = None;
    loop {
        let scan = CacheGcScan {
            start_at: None,
            detail: detail.as_deref(),
            keep_going: None,
        };
        prune_worktree_cache(
            &root,
            &cache_root,
            name,
            &CacheGcPolicy::default(),
            &mut report,
            &mut |path| Err(missing_custody(path)),
            false,
            &scan,
            detail.as_deref(),
        )?;
        if report.interrupted_at.is_none() {
            break;
        }
        detail = report.continuation.take();
        report.interrupted_at = None;
    }
    Ok(report.bytes_freed)
}

/// Observe the private cache which an eligible worktree removal will orphan.
/// The caller holds worktree custody; the same allocated-tree measure is used
/// after removal. Shared Cargo download links are never followed.
pub(crate) fn preview_worktree_cargo_cache(
    home: &Path,
    worktree: &crate::repo::VerifiedWorktree,
) -> Result<Option<CacheGcEntry>, PermissionError> {
    let path = crate::permissions::worktree_cargo_cache_path(home, worktree)?;
    let cache_root = home.join("Library/Caches/provenant-lane-cargo");
    let Some(root) = open_cache_root(&cache_root)? else {
        return Ok(None);
    };
    let name = path.file_name().ok_or(PermissionError::AnchorMismatch)?;
    let directory = match root.open_verified_directory(name) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(cache_fs_error("open private cache", &path)(error)),
    };
    verify_cache_metadata(
        &directory
            .metadata()
            .map_err(cache_fs_error("stat private cache", &path))?,
    )?;
    let Some(owner) = read_cache_owner(&directory, &path)? else {
        return Ok(None);
    };
    let identity = worktree.identity();
    if owner.path != worktree.path() || owner.identity != (identity.device, identity.inode) {
        return Err(PermissionError::AnchorMismatch);
    }
    let measured = measure_tree_with_progress(&directory, &path, None, &|| true)?;
    let Some(stats) = measured.stats else {
        return Ok(None);
    };
    Ok(Some(CacheGcEntry {
        path,
        bytes: stats.allocated_bytes,
        reason: CacheGcReason::Orphaned,
    }))
}

/// Sweep the lane cache root: orphaned caches always, idle ones when the policy
/// sets a threshold and lists the owner in this project. Retry GC tombstones
/// too. Without a writer guard, idle and free-floor caches are kept as
/// unobservable.
/// Directories are opened without following symlinks or crossing devices.
pub fn gc_worktree_cargo_caches(
    home: &Path,
    policy: &CacheGcPolicy,
    scan: &CacheGcScan<'_>,
) -> Result<CacheGcReport, CacheGcError> {
    gc_worktree_cargo_caches_with_writer_guard(
        home,
        policy,
        |path| Err(missing_custody(path)),
        scan,
    )
}

fn missing_custody(path: &Path) -> Occupancy {
    Occupancy::custody_unknown(path.to_path_buf(), "writer custody was not established")
}

fn guard_reason(state: &Occupancy) -> CacheGcReason {
    match state {
        Occupancy::Held => CacheGcReason::Busy,
        Occupancy::Observed { .. } => CacheGcReason::Live,
        _ => CacheGcReason::Unobservable,
    }
}

type CacheWriterGuard<'a> = dyn FnMut(&Path) -> Result<Option<LockGuard>, Occupancy> + 'a;

/// Sweep caches while holding the owner's writer lock across idle or free-floor
/// detachment.
/// The guard must establish writer exclusion or refuse unobservable state.
pub fn gc_worktree_cargo_caches_with_writer_guard(
    home: &Path,
    policy: &CacheGcPolicy,
    writer_guard: impl FnMut(&Path) -> Result<Option<LockGuard>, Occupancy>,
    scan: &CacheGcScan<'_>,
) -> Result<CacheGcReport, CacheGcError> {
    gc_caches_with_free_space(home, policy, writer_guard, scan, |path| {
        provenant_os::fs::Directory::open_anchor(path)
            .map_err(std::io::Error::other)?
            .available_bytes()
    })
}

fn gc_caches_with_free_space(
    home: &Path,
    policy: &CacheGcPolicy,
    mut writer_guard: impl FnMut(&Path) -> Result<Option<LockGuard>, Occupancy>,
    scan: &CacheGcScan<'_>,
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
        sweep_cache_root(
            &root,
            &cache_root,
            policy,
            &mut report,
            &mut writer_guard,
            scan,
        )?;
        if report.interrupted_at.is_some() {
            return Ok(());
        }
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
                // Free-space pressure may remove idle caches, but never a cache
                // newer than the configured idle lifetime.
                let mut pressure = policy.clone();
                let mut candidates = Vec::new();
                let mut entries = root
                    .entries()
                    .map_err(cache_fs_error("opendir", &cache_root))?;
                while let Some(entry) = entries.next() {
                    if !scan.permits() {
                        report.interrupted_at = Some(cache_root.clone());
                        report.bytes_estimated = true;
                        return Ok(());
                    }
                    let name = entry.map_err(cache_fs_error("readdir", &cache_root))?;
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
                    if let Some((reason, stats)) =
                        cache_gc_reason(&cache, &path, &owner, &pressure, scan, None, &mut report)?
                    {
                        if reason == CacheGcReason::Idle {
                            candidates.push((stats.newest_modified_ns, name));
                        }
                    }
                    if report.interrupted_at.is_some() {
                        return Ok(());
                    }
                }
                candidates.sort();
                for (newest, name) in candidates {
                    if !scan.permits() {
                        report.interrupted_at = Some(cache_root.join(&name));
                        return Ok(());
                    }
                    if available >= floor {
                        break;
                    }
                    // Recheck against the observed last write while holding writer exclusion.
                    pressure.idle_before_ns = Some(
                        policy
                            .idle_before_ns
                            .map_or(newest.saturating_add(1), |threshold| {
                                threshold.min(newest.saturating_add(1))
                            }),
                    );
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
                        scan,
                        None,
                    )?;
                    if report.interrupted_at.is_some() {
                        return Ok(());
                    }
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
        if report.interrupted_at.is_some() {
            return Ok(());
        }
        collect_total_cap(
            &root,
            &cache_root,
            policy,
            &mut report,
            &mut writer_guard,
            scan,
        )?;
        if policy.max_total_bytes.is_some() && policy.free_floor_bytes.is_some() {
            report.free_bytes_after = Some(if policy.dry_run {
                report
                    .free_bytes_before
                    .unwrap_or(0)
                    .saturating_add(report.bytes_freed)
            } else {
                free_space(&cache_root)
                    .map_err(|error| cache_io_error("read free space", &cache_root, error))?
            });
        }
        Ok(())
    })();
    match sweep {
        Ok(()) => Ok(report),
        Err(source) => {
            if report.errors.is_empty() {
                let root = home.join("Library/Caches/provenant-lane-cargo");
                let (path, operation, cause) = match &source {
                    PermissionError::CacheSweepFilesystem {
                        operation,
                        path,
                        source,
                    } => (root.join(path), (*operation).to_owned(), source.to_string()),
                    error => (root, "collect cache class".into(), error.to_string()),
                };
                report.errors.push(RetentionIssue {
                    path,
                    operation,
                    cause,
                });
            }
            Err(CacheGcError {
                report: Box::new(report),
                source: Box::new(source),
            })
        }
    }
}

/// Apply the total bound after ordinary orphan/age collection. Count protected
/// and foreign caches too, but only the existing writer-guarded idle path may
/// detach an extant worktree's cache.
fn collect_total_cap(
    root: &Directory,
    cache_root: &Path,
    policy: &CacheGcPolicy,
    report: &mut CacheGcReport,
    writer_guard: &mut CacheWriterGuard<'_>,
    scan: &CacheGcScan<'_>,
) -> Result<(), PermissionError> {
    let Some(cap) = policy.max_total_bytes else {
        return Ok(());
    };
    // A byte cap does not make a recently used cache cold. Leave the cap
    // unsatisfied when all eligible caches are still within their idle window.
    let mut pressure = policy.clone();
    pressure.free_floor_bytes = None;
    let mut total = 0_u64;
    let mut candidates = Vec::new();
    let mut entries = root
        .entries()
        .map_err(cache_fs_error("opendir", cache_root))?;
    while let Some(entry) = entries.next() {
        if !scan.permits() {
            report.interrupted_at = Some(cache_root.to_owned());
            report.bytes_estimated = true;
            return Ok(());
        }
        let name = entry.map_err(cache_fs_error("readdir", cache_root))?;
        let path = cache_root.join(&name);
        if !name
            .to_str()
            .is_some_and(|name| is_cache_digest(name) || cache_tombstone(name).is_some())
            || report.removed.iter().any(|entry| entry.path == path)
        {
            continue;
        }
        let metadata = root
            .metadata_at(&name, false)
            .map_err(cache_fs_error("stat", Path::new(&name)))?;
        if metadata.kind != FileKind::Directory {
            continue;
        }
        let cache = root
            .open_verified_directory(&name)
            .map_err(cache_fs_error("open verified directory", Path::new(&name)))?;
        let progress = measure_cache(&cache, &path, None, scan)?;
        let Some(stats) = progress.stats else {
            report.interrupted_at = Some(path);
            report.continuation = progress.continuation;
            report.bytes_estimated = true;
            return Ok(());
        };
        let newest = stats.newest_modified_ns;
        total = total.saturating_add(stats.allocated_bytes);
        if metadata.uid != provenant_os::fs::effective_uid() || metadata.mode & 0o7777 != 0o700 {
            continue;
        }
        let Some(owner) = read_cache_owner(&cache, Path::new(&name))? else {
            continue;
        };
        if cache_gc_reason_with_newest(Path::new(&name), &owner, &pressure, || Ok(newest))?
            == Some(CacheGcReason::Idle)
        {
            candidates.push((newest, name));
        }
    }
    candidates.sort();
    for (newest, name) in candidates {
        if total <= cap {
            break;
        }
        pressure.idle_before_ns = Some(
            policy
                .idle_before_ns
                .map_or(newest.saturating_add(1), |threshold| {
                    threshold.min(newest.saturating_add(1))
                }),
        );
        let before = report.bytes_freed;
        prune_worktree_cache(
            root,
            cache_root,
            &name,
            &pressure,
            report,
            writer_guard,
            true,
            scan,
            None,
        )?;
        if report.interrupted_at.is_some() {
            return Ok(());
        }
        total = total.saturating_sub(report.bytes_freed.saturating_sub(before));
    }
    report.total_bytes_after = Some(total);
    Ok(())
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
    scan: &CacheGcScan<'_>,
) -> Result<(), PermissionError> {
    let mut failures = Vec::new();
    let root_metadata = root
        .metadata()
        .map_err(cache_fs_error("stat cache root", cache_root))?;
    let mut root_stamp = TreeMeasureMetadata::from(&root_metadata);
    let mut entries = root
        .entries()
        .map_err(cache_fs_error("opendir", cache_root))?;
    let cursor = scan
        .detail
        .filter(|detail| detail.len() <= MAX_CACHE_CURSOR_BYTES)
        .and_then(|detail| serde_json::from_str::<CacheGcCursor>(detail).ok());

    let mut current = None;
    let mut next = None;
    let mut measure_detail = None;
    let mut root_cookie_valid = false;
    match cursor {
        Some(CacheGcCursor::Root { metadata, cookie })
            if scan.start_at == Some(cache_root) && metadata.matches(&root_metadata) =>
        {
            entries
                .seek(cookie)
                .map_err(cache_fs_error("seek cache root", cache_root))?;
            root_cookie_valid = true;
        }
        Some(CacheGcCursor::RootSearch {
            metadata,
            cookie,
            target,
        }) if metadata.matches(&root_metadata) => {
            entries
                .seek(cookie)
                .map_err(cache_fs_error("seek cache root", cache_root))?;
            if let Some(target) = decode_tree_name(&target) {
                match find_cache_entry(root, cache_root, &mut entries, &target, scan, report)? {
                    Some(found) => {
                        current = Some(found);
                        root_cookie_valid = true;
                    }
                    None if report.interrupted_at.is_some() => return Ok(()),
                    None => {
                        entries = root
                            .entries()
                            .map_err(cache_fs_error("opendir", cache_root))?;
                    }
                }
            } else {
                entries = root
                    .entries()
                    .map_err(cache_fs_error("opendir", cache_root))?;
            }
        }
        Some(CacheGcCursor::Candidate {
            root_metadata: saved_root,
            cookie,
            current: saved_current,
            next: saved_next,
            measure,
        }) if saved_root.matches(&root_metadata)
            && scan
                .start_at
                .and_then(Path::file_name)
                .is_some_and(|name| encode_tree_name(name) == saved_current) =>
        {
            entries
                .seek(cookie)
                .map_err(cache_fs_error("seek cache root", cache_root))?;
            current = decode_tree_name(&saved_current);
            next = saved_next.as_deref().and_then(decode_tree_name);
            measure_detail = measure;
            root_cookie_valid = true;
        }
        _ => {}
    }

    if current.is_none() && !root_cookie_valid {
        if let Some(name) = scan.start_at.and_then(Path::file_name)
            && scan.start_at != Some(cache_root)
        {
            match find_cache_entry(root, cache_root, &mut entries, name, scan, report)? {
                Some(found) => current = Some(found),
                None if report.interrupted_at.is_some() => return Ok(()),
                None => {
                    entries = root
                        .entries()
                        .map_err(cache_fs_error("opendir", cache_root))?;
                }
            }
        }
    }

    loop {
        if current.is_none() {
            if !scan.permits() {
                let cookie = entries
                    .position()
                    .map_err(cache_fs_error("position cache root", cache_root))?;
                set_cache_gc_cursor(
                    report,
                    cache_root,
                    CacheGcCursor::Root {
                        metadata: root_stamp,
                        cookie,
                    },
                );
                report.bytes_estimated = true;
                return Ok(());
            }
            current = match entries.next() {
                Some(Ok(name)) => Some(name),
                Some(Err(error)) => {
                    failures.push((None, cache_fs_error("readdir", cache_root)(error)));
                    continue;
                }
                None => break,
            };
        }

        let name = current.take().ok_or(PermissionError::AnchorMismatch)?;
        if next.is_none() && scan.permits() {
            match entries.next() {
                Some(Ok(name)) => next = Some(name),
                Some(Err(error)) => {
                    failures.push((Some(name), cache_fs_error("readdir", cache_root)(error)));
                    continue;
                }
                None => {}
            }
        }
        let cookie = entries
            .position()
            .map_err(cache_fs_error("position cache root", cache_root))?;
        let before_removed = report.removed.len();
        let before_interrupted = report.interrupted_at.is_some();
        if let Err(error) = prune_worktree_cache(
            root,
            cache_root,
            &name,
            policy,
            report,
            writer_guard,
            false,
            scan,
            measure_detail.as_deref(),
        ) {
            failures.push((Some(name.clone()), error));
        }
        measure_detail = None;
        if report.interrupted_at.is_some() && !before_interrupted {
            if report.continuation.as_deref() == Some("delete") {
                report.continuation = encode_cache_gc_cursor(CacheGcCursor::Delete);
            } else {
                report.continuation = encode_cache_gc_cursor(CacheGcCursor::Candidate {
                    root_metadata: root_stamp,
                    cookie,
                    current: encode_tree_name(&name),
                    next: next.as_deref().map(encode_tree_name),
                    measure: report.continuation.take(),
                });
            }
            report.bytes_estimated = true;
            return Ok(());
        }

        let after_metadata = root
            .metadata()
            .map_err(cache_fs_error("stat cache root", cache_root))?;
        let root_changed = !root_stamp.matches(&after_metadata);
        if report.removed.len() > before_removed || root_changed {
            let Some(next_name) = next.take() else {
                break;
            };
            // Removing or detaching a cache changes the root directory, so its
            // old cookie is no longer valid. Reopen and find the saved next
            // entry by name before continuing this sweep.
            entries = root
                .entries()
                .map_err(cache_fs_error("opendir", cache_root))?;
            root_stamp = TreeMeasureMetadata::from(&after_metadata);
            let Some(found) =
                find_cache_entry(root, cache_root, &mut entries, &next_name, scan, report)?
            else {
                if report.interrupted_at.is_some() {
                    return Ok(());
                }
                entries = root
                    .entries()
                    .map_err(cache_fs_error("opendir", cache_root))?;
                continue;
            };
            current = Some(found);
            continue;
        }
        current = next.take();
    }

    for (name, error) in &failures {
        let (path, operation, cause) = match error {
            PermissionError::CacheSweepFilesystem {
                operation,
                path,
                source,
            } => (
                cache_root.join(path),
                (*operation).to_owned(),
                source.to_string(),
            ),
            error => (
                cache_root.join(name.as_deref().unwrap_or(OsStr::new("."))),
                "collect cache".into(),
                error.to_string(),
            ),
        };
        report.errors.push(RetentionIssue {
            path,
            operation,
            cause,
        });
    }
    if !failures.is_empty() {
        return Err(PermissionError::CacheSweepIo(std::io::Error::other(
            CacheSweepFailures(failures),
        )));
    }
    Ok(())
}

fn find_cache_entry(
    root: &Directory,
    cache_root: &Path,
    entries: &mut Entries,
    target: &OsStr,
    scan: &CacheGcScan<'_>,
    report: &mut CacheGcReport,
) -> Result<Option<OsString>, PermissionError> {
    while scan.permits() {
        let Some(entry) = entries.next() else {
            return Ok(None);
        };
        let name = entry.map_err(cache_fs_error("readdir", cache_root))?;
        if name == target {
            return Ok(Some(name));
        }
    }
    let cookie = entries
        .position()
        .map_err(cache_fs_error("position cache root", cache_root))?;
    let metadata = root
        .metadata()
        .map_err(cache_fs_error("stat cache root", cache_root))?;
    set_cache_gc_cursor(
        report,
        cache_root,
        CacheGcCursor::RootSearch {
            metadata: TreeMeasureMetadata::from(&metadata),
            cookie,
            target: encode_tree_name(target),
        },
    );
    report.bytes_estimated = true;
    Ok(None)
}

fn set_cache_gc_cursor(report: &mut CacheGcReport, path: &Path, cursor: CacheGcCursor) {
    report.interrupted_at = Some(path.to_owned());
    report.continuation = encode_cache_gc_cursor(cursor);
}

fn encode_cache_gc_cursor(cursor: CacheGcCursor) -> Option<String> {
    serde_json::to_string(&cursor)
        .ok()
        .filter(|detail| detail.len() <= MAX_CACHE_CURSOR_BYTES)
}

#[derive(Debug)]
struct CacheSweepFailures(Vec<(Option<OsString>, PermissionError)>);

impl std::fmt::Display for CacheSweepFailures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, (name, error)) in self.0.iter().enumerate() {
            if index > 0 {
                write!(f, "; ")?;
            }
            if let Some(name) = name {
                write!(f, "{name:?}: ")?;
            }
            write!(f, "{error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CacheSweepFailures {}

#[allow(
    clippy::too_many_arguments,
    reason = "cache identity, custody and shared measurement budget stay explicit at the mutation boundary"
)]
fn prune_worktree_cache(
    root: &provenant_os::fs::Directory,
    cache_root: &Path,
    name: &OsStr,
    policy: &CacheGcPolicy,
    report: &mut CacheGcReport,
    writer_guard: &mut CacheWriterGuard<'_>,
    free_floor: bool,
    scan: &CacheGcScan<'_>,
    detail: Option<&str>,
) -> Result<(), PermissionError> {
    let pressure_reason = |reason| match reason {
        Some(CacheGcReason::Idle) if free_floor => Some(
            if policy.max_total_bytes.is_some() && policy.free_floor_bytes.is_none() {
                CacheGcReason::TotalCap
            } else {
                CacheGcReason::FreeFloor
            },
        ),
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
    let Some(_cargo_guards) = lock_cargo_profiles(&cache, &|| scan.permits()) else {
        if !scan.permits() {
            report.interrupted_at = Some(cache_root.join(name));
            report.continuation = None;
            return Ok(());
        }
        report.kept.push(CacheGcEntry {
            path: cache_root.join(name),
            bytes: 0,
            reason: CacheGcReason::Busy,
        });
        return Ok(());
    };
    let (reason, measured) = if let Some((device, inode, fingerprint)) = tombstone {
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
        (CacheGcReason::Tombstone, None)
    } else {
        let Some(owner) = owner.as_ref() else {
            return Ok(());
        };
        let Some((reason, stats)) = cache_gc_reason(
            &cache,
            &cache_root.join(name),
            owner,
            policy,
            scan,
            detail,
            report,
        )?
        else {
            if report.interrupted_at.is_some() {
                return Ok(());
            }
            if let Some(state) = policy.protected_owners.get(&owner.path) {
                report.kept.push(CacheGcEntry {
                    path: cache_root.join(name),
                    bytes: 0,
                    reason: match state {
                        Occupancy::Observed { .. } => CacheGcReason::Live,
                        Occupancy::Held => CacheGcReason::Busy,
                        _ => CacheGcReason::Unobservable,
                    },
                });
            }
            return Ok(());
        };
        let reason = pressure_reason(Some(reason)).unwrap_or(reason);
        if reason == CacheGcReason::Foreign {
            report.kept.push(CacheGcEntry {
                path: cache_root.join(name),
                bytes: 0,
                reason,
            });
            return Ok(());
        }
        (reason, Some(stats))
    };
    let bytes = measured.map_or(0, |stats| stats.allocated_bytes);
    if !scan.permits() {
        report.interrupted_at = Some(cache_root.join(name));
        report.continuation = None;
        report.bytes_estimated = true;
        return Ok(());
    }
    if policy.dry_run {
        if matches!(
            reason,
            CacheGcReason::Idle | CacheGcReason::FreeFloor | CacheGcReason::TotalCap
        ) {
            let owner = owner.as_ref().ok_or(PermissionError::AnchorMismatch)?;
            // Probe only: a dry run never needs to hold writer exclusion.
            if let Err(state) = writer_guard(&owner.path) {
                report.kept.push(CacheGcEntry {
                    path: cache_root.join(name),
                    bytes: 0,
                    reason: guard_reason(&state),
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
            let idle_guard = if matches!(
                reason,
                CacheGcReason::Idle | CacheGcReason::FreeFloor | CacheGcReason::TotalCap
            ) {
                match writer_guard(&original_owner.path)
                    .and_then(|guard| guard.ok_or_else(|| missing_custody(&original_owner.path)))
                {
                    Ok(guard) => Some(guard),
                    Err(state) => {
                        report.kept.push(CacheGcEntry {
                            path: cache_root.join(name),
                            bytes: 0,
                            reason: guard_reason(&state),
                        });
                        return Ok(());
                    }
                }
            } else {
                None
            };
            if !scan.permits() {
                report.interrupted_at = Some(cache_root.join(name));
                report.continuation = None;
                return Ok(());
            }
            // Measurement may take time. Recheck the marker, owner and last
            // write immediately before detaching the cache from the lane path.
            if read_cache_owner(&cache, Path::new(name))? != owner {
                return Ok(());
            }
            let newest = measured.map_or(i128::MIN, |stats| stats.newest_modified_ns);
            let current_reason = pressure_reason(cache_gc_reason_with_newest(
                &cache_root.join(name),
                original_owner,
                policy,
                || Ok(newest),
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
        let before_remove = report.bytes_freed;
        let complete = remove_cache_contents(
            &detached_cache,
            Path::new(removal_name),
            &mut report.bytes_freed,
            scan,
        )?;
        if !complete {
            report.interrupted_at = Some(cache_root.join(removal_name));
            report.continuation = Some("delete".into());
            report.bytes_estimated = true;
            return Ok(());
        }
        // The top-level marker survives every earlier failure.
        let marker_name = OsStr::new(CARGO_CACHE_OWNER);
        if !scan.permits() {
            report.interrupted_at = Some(cache_root.join(removal_name));
            report.continuation = Some("delete".into());
            return Ok(());
        }
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
                report.bytes_freed = report.bytes_freed.saturating_add(marker.allocated);
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => {
                return Err(cache_fs_error(
                    "stat",
                    &Path::new(removal_name).join(marker_name),
                )(error));
            }
        }
        if !scan.permits() {
            report.interrupted_at = Some(cache_root.join(removal_name));
            report.continuation = Some("delete".into());
            return Ok(());
        }
        cache_gc_operation("rmdir", Path::new(removal_name), || {
            root.rmdir(removal_name)
        })
        .map_err(cache_fs_error("rmdir", Path::new(removal_name)))?;
        report.bytes_freed = report.bytes_freed.saturating_add(opened.allocated);
        let _ = before_remove;
    }
    report.removed.push(CacheGcEntry {
        path: cache_root.join(name),
        bytes,
        reason,
    });
    Ok(())
}

/// Hold all Cargo profile and target owner locks under one cache root. Shared
/// project targets use the same probe as private lane caches.
pub fn lock_cargo_profiles(
    cache: &Directory,
    keep_going: &dyn Fn() -> bool,
) -> Option<Vec<LockGuard>> {
    fn visit(
        dir: &Directory,
        depth: usize,
        guards: &mut Vec<LockGuard>,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<()> {
        if !keep_going() {
            return None;
        }
        let mut profile = false;
        for name in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
            if !keep_going() {
                return None;
            }
            match dir.try_flock_foreign(OsStr::new(name), LockMode::Exclusive) {
                Ok(ForeignLock::Held) | Err(_) => return None,
                Ok(ForeignLock::Acquired(guard)) => {
                    profile = true;
                    guards.push(guard);
                }
                Ok(ForeignLock::Absent) => {}
            }
        }
        if profile || depth == 2 {
            return Some(());
        }
        let mut entries = dir.entries().ok()?;
        let mut count = 0_usize;
        while let Some(entry) = entries.next() {
            if !keep_going() {
                return None;
            }
            count += 1;
            if count > 1000 {
                return None;
            }
            let name = entry.ok()?;
            let meta = dir.metadata_at(&name, false).ok()?;
            if meta.kind == FileKind::Directory {
                visit(
                    &dir.open_verified_directory(&name).ok()?,
                    depth + 1,
                    guards,
                    keep_going,
                )?;
            }
        }
        Some(())
    }
    let mut guards = Vec::new();
    if !keep_going() {
        return None;
    }
    match cache.try_flock_foreign(OsStr::new(CARGO_CACHE_OWNER), LockMode::Exclusive) {
        Ok(ForeignLock::Held) | Err(_) => return None,
        Ok(ForeignLock::Acquired(guard)) => guards.push(guard),
        Ok(ForeignLock::Absent) => {}
    }
    for target_name in ["target", "target-lint"] {
        if !keep_going() {
            return None;
        }
        match cache.open_verified_directory(OsStr::new(target_name)) {
            Ok(target) => visit(&target, 0, &mut guards, keep_going)?,
            Err(error) if error.is_not_found() => {}
            Err(_) => return None,
        }
    }
    Some(guards)
}

/// Read the verified worktree path that owns a private lane cache.
pub fn lane_cargo_cache_owner_path(
    cache: &Directory,
) -> Result<Option<std::path::PathBuf>, PermissionError> {
    Ok(read_cache_owner(cache, Path::new("."))?.map(|owner| owner.path))
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

fn encode_owner_marker(version: &[u8], fields: &[u8]) -> Vec<u8> {
    let mut marker = Vec::with_capacity(version.len() + 1 + fields.len());
    marker.extend_from_slice(version);
    marker.push(b'\n');
    marker.extend_from_slice(fields);
    marker
}

pub(crate) fn encode_cache_owner(owner: &CacheOwner) -> Vec<u8> {
    let mut fields = Vec::with_capacity(16 + owner.path.as_os_str().as_encoded_bytes().len());
    fields.extend_from_slice(&owner.identity.0.to_le_bytes());
    fields.extend_from_slice(&owner.identity.1.to_le_bytes());
    fields.extend_from_slice(owner.path.as_os_str().as_encoded_bytes());
    encode_owner_marker(CARGO_CACHE_OWNER_VERSION, &fields)
}

#[cfg(test)]
pub(crate) fn owner_marker(version: &[u8], fields: &[u8]) -> Vec<u8> {
    encode_owner_marker(version, fields)
}

pub(crate) fn cache_owner_fingerprint(owner: &CacheOwner) -> String {
    let mut digest = Sha256::new();
    digest.update(encode_cache_owner(owner));
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
    scan: &CacheGcScan<'_>,
    detail: Option<&str>,
    report: &mut CacheGcReport,
) -> Result<Option<(CacheGcReason, TreeStats)>, PermissionError> {
    let mut measured = None;
    let reason = cache_gc_reason_with_newest(relative, owner, policy, || {
        let progress = measure_cache(cache, relative, detail, scan)?;
        let Some(stats) = progress.stats else {
            report.interrupted_at = Some(relative.to_owned());
            report.continuation = progress.continuation;
            report.bytes_estimated = true;
            return Ok(i128::MAX);
        };
        measured = Some(stats);
        Ok(stats.newest_modified_ns)
    })?;
    if report.interrupted_at.is_some() {
        return Ok(None);
    }
    let Some(reason) = reason else {
        return Ok(None);
    };
    let stats = if let Some(stats) = measured {
        stats
    } else {
        let progress = measure_cache(cache, relative, detail, scan)?;
        let Some(stats) = progress.stats else {
            report.interrupted_at = Some(relative.to_owned());
            report.continuation = progress.continuation;
            report.bytes_estimated = true;
            return Ok(None);
        };
        stats
    };
    Ok(Some((reason, stats)))
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

    // Occupancy is stronger than cache age and owner state. In particular, a
    // busy cache whose worktree disappeared during a run remains protected.
    if policy.protected_owners.contains_key(&owner.path) {
        return Ok(None);
    }

    let reason = match std::fs::symlink_metadata(&owner.path) {
        Ok(_) if !policy.project_owners.contains(&owner.path) => {
            if let Some(threshold) = policy.idle_before_ns
                && newest_write_ns()? >= threshold
            {
                return Ok(None);
            }
            CacheGcReason::Foreign
        }
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
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(threshold) = policy.idle_before_ns
                && newest_write_ns()? >= threshold
            {
                return Ok(None);
            }
            CacheGcReason::Orphaned
        }
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
    detail: Option<&str>,
    scan: &CacheGcScan<'_>,
) -> Result<TreeMeasureProgress, PermissionError> {
    measure_tree_with_progress(directory, relative, detail, &|| scan.permits())
}

fn remove_cache_contents(
    directory: &PayloadTree,
    relative: &Path,
    bytes_freed: &mut u64,
    scan: &CacheGcScan<'_>,
) -> Result<bool, PermissionError> {
    remove_cache_contents_at(directory, relative, 0, bytes_freed, scan)
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
    scan: &CacheGcScan<'_>,
) -> Result<bool, PermissionError> {
    if depth >= MAX_CACHE_SWEEP_DEPTH {
        return Err(PermissionError::CacheSweepDepth);
    }
    if !scan.permits() {
        return Ok(false);
    }
    directory
        .make_writable()
        .map_err(cache_fs_error("make cache writable", relative))?;
    loop {
        if !scan.permits() {
            return Ok(false);
        }
        let mut names = Vec::with_capacity(CACHE_DELETE_BATCH_ENTRIES);
        for entry in cache_gc_entries(directory.directory(), relative)
            .map_err(cache_fs_error("readdir", relative))?
            .take(CACHE_DELETE_BATCH_ENTRIES)
        {
            if !scan.permits() {
                return Ok(false);
            }
            let name = entry.map_err(cache_fs_error("readdir", relative))?;
            if depth == 0 && name == CARGO_CACHE_OWNER {
                continue;
            }
            names.push(name);
        }
        if names.is_empty() {
            return Ok(true);
        }
        for name in names {
            if !scan.permits() {
                return Ok(false);
            }
            let path = relative.join(&name);
            let metadata = cache_gc_operation("stat", &relative.join(&name), || {
                directory.metadata_at(&name)
            })
            .map_err(cache_fs_error("stat", &path))?;
            if metadata.kind == FileKind::Directory {
                directory
                    .directory()
                    .make_owner_accessible_at(&name)
                    .map_err(cache_fs_error("make cache directory accessible", &path))?;
                let child = cache_gc_operation("open verified", &relative.join(&name), || {
                    directory.child(&name)
                })
                .map_err(cache_fs_error("open verified directory", &path))?;
                if !remove_cache_contents_at(&child, &path, depth + 1, bytes_freed, scan)? {
                    return Ok(false);
                }
                drop(child);
                if !scan.permits() {
                    return Ok(false);
                }
                cache_gc_operation("rmdir", &relative.join(&name), || directory.rmdir(&name))
                    .map_err(cache_fs_error("rmdir", &path))?;
            } else {
                if !scan.permits() {
                    return Ok(false);
                }
                cache_gc_operation("unlink", &relative.join(&name), || directory.unlink(&name))
                    .map_err(cache_fs_error("unlink", &path))?;
            }
            *bytes_freed = bytes_freed.saturating_add(metadata.allocated);
        }
    }
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

    fn gc_worktree_cargo_caches(
        home: &Path,
        policy: &CacheGcPolicy,
    ) -> Result<CacheGcReport, CacheGcError> {
        super::gc_worktree_cargo_caches(home, policy, &CacheGcScan::default())
    }

    fn gc_worktree_cargo_caches_with_writer_guard(
        home: &Path,
        policy: &CacheGcPolicy,
        writer_guard: impl FnMut(&Path) -> Result<Option<LockGuard>, Occupancy>,
    ) -> Result<CacheGcReport, CacheGcError> {
        super::gc_worktree_cargo_caches_with_writer_guard(
            home,
            policy,
            writer_guard,
            &CacheGcScan::default(),
        )
    }

    fn gc_caches_with_free_space(
        home: &Path,
        policy: &CacheGcPolicy,
        writer_guard: impl FnMut(&Path) -> Result<Option<LockGuard>, Occupancy>,
        free_space: impl FnMut(&Path) -> std::io::Result<u64>,
    ) -> Result<CacheGcReport, CacheGcError> {
        super::gc_caches_with_free_space(
            home,
            policy,
            writer_guard,
            &CacheGcScan::default(),
            free_space,
        )
    }

    fn prune_orphaned_worktree_cargo_caches(home: &Path) -> Result<usize, CacheGcError> {
        super::prune_orphaned_worktree_cargo_caches(home, &CacheGcScan::default())
    }

    #[test]
    fn tree_measurement_resumes_from_a_bounded_directory_cursor()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let tree = temporary.path().join("tree");
        let nested = tree.join("nested");
        std::fs::create_dir_all(&nested)?;
        for index in 0..100 {
            std::fs::write(
                nested.join(format!("file-{index:03}")),
                vec![index as u8; 128],
            )?;
        }
        let directory = Directory::open_anchor(&tree)?;
        let expected = measure_tree_with_progress(&directory, &tree, None, &|| true)?
            .stats
            .ok_or("complete baseline measurement")?;

        let polls = std::cell::Cell::new(0);
        let keep_going = || {
            let count = polls.get();
            polls.set(count + 1);
            count < 20
        };
        let partial = measure_tree_with_progress(&directory, &tree, None, &keep_going)?;
        assert_eq!(partial.stats, None);
        assert_eq!(partial.interrupted_at.as_deref(), Some(tree.as_path()));
        let mut detail = partial.continuation.ok_or("measurement cursor")?;

        let resumed = measure_tree_with_progress(&directory, &tree, Some(&detail), &|| true)?;
        if resumed.stats.is_none() {
            detail = resumed.continuation.ok_or("resumed measurement cursor")?;
        }
        let completed = if let Some(stats) = resumed.stats {
            stats
        } else {
            measure_tree_with_progress(&directory, &tree, Some(&detail), &|| true)?
                .stats
                .ok_or("complete resumed measurement")?
        };
        assert_eq!(completed, expected);
        Ok(())
    }

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
            let marker = encode_cache_owner(&CacheOwner {
                path: owner,
                identity: (identity.dev(), identity.ino()),
            });
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
        let marker = encode_cache_owner(&CacheOwner {
            path: missing_owner,
            identity: (1, 2),
        });
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
            sweep_with_scan(home, policy, event, &CacheGcScan::default())
        }

        fn sweep_with_writer_guard(
            home: &Path,
            policy: &CacheGcPolicy,
            event: Event,
            writer_guard: &mut CacheWriterGuard<'_>,
        ) -> Result<CacheGcReport, CacheGcError> {
            sweep_with_writer_guard_and_scan(
                home,
                policy,
                event,
                writer_guard,
                &CacheGcScan::default(),
            )
        }

        fn sweep_with_scan(
            home: &Path,
            policy: &CacheGcPolicy,
            event: Event,
            scan: &CacheGcScan<'_>,
        ) -> Result<CacheGcReport, CacheGcError> {
            sweep_with_writer_guard_and_scan(
                home,
                policy,
                event,
                &mut |path| Err(missing_custody(path)),
                scan,
            )
        }

        fn sweep_with_writer_guard_and_scan(
            home: &Path,
            policy: &CacheGcPolicy,
            event: Event,
            writer_guard: &mut CacheWriterGuard<'_>,
            scan: &CacheGcScan<'_>,
        ) -> Result<CacheGcReport, CacheGcError> {
            let path = home.join("Library/Caches/provenant-lane-cargo");
            let mut report = CacheGcReport {
                dry_run: policy.dry_run,
                ..CacheGcReport::default()
            };
            let _event = CacheGcEvent::install(path.clone(), event);
            let result = Directory::open_anchor(&path)
                .map_err(PermissionError::Filesystem)
                .and_then(|root| {
                    sweep_cache_root(&root, &path, policy, &mut report, writer_guard, scan)
                });
            result
                .map(|()| report.clone())
                .map_err(|source| CacheGcError {
                    report: Box::new(report),
                    source: Box::new(source),
                })
        }

        fn cache(home: &Path, name: &str, owner: &Path) -> io::Result<PathBuf> {
            let path = home.join("Library/Caches/provenant-lane-cargo").join(name);
            std::fs::create_dir_all(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            let identity = std::fs::metadata(owner).ok();
            let marker = encode_cache_owner(&CacheOwner {
                path: owner.to_path_buf(),
                identity: (
                    identity.as_ref().map_or(1, MetadataExt::dev),
                    identity.as_ref().map_or(2, MetadataExt::ino),
                ),
            });
            std::fs::write(path.join(CARGO_CACHE_OWNER), marker)?;
            std::fs::write(path.join("artifact"), b"cached build")?;
            Ok(path)
        }

        #[test]
        fn interrupted_root_sweep_resumes_without_restarting_completed_entries()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let mut paths = Vec::new();
            for index in 0..12 {
                let owner = home.join(format!("missing-owner-{index}"));
                paths.push(cache(home, &format!("{index:064x}"), &owner)?);
            }

            let polls = std::cell::Cell::new(0);
            let keep_going = || {
                let count = polls.get();
                polls.set(count + 1);
                count < 80
            };
            let first_scan = CacheGcScan {
                keep_going: Some(&keep_going),
                ..CacheGcScan::default()
            };
            let first = sweep_with_scan(
                home,
                &CacheGcPolicy::default(),
                Arc::new(|_, _| Ok(())),
                &first_scan,
            )?;
            let start_at = first
                .interrupted_at
                .clone()
                .ok_or("bounded root sweep interruption")?;
            let detail = first.continuation.clone();

            let resume_scan = CacheGcScan {
                start_at: Some(&start_at),
                detail: detail.as_deref(),
                ..CacheGcScan::default()
            };
            let resumed = sweep_with_scan(
                home,
                &CacheGcPolicy::default(),
                Arc::new(|_, _| Ok(())),
                &resume_scan,
            )?;
            assert!(resumed.interrupted_at.is_none());
            assert!(paths.iter().all(|path| !path.exists()));
            Ok(())
        }

        #[test]
        fn partial_delete_keeps_owner_marker_and_releases_writer_lock()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let path = cache(home, &"a".repeat(64), &home.join("missing-owner"))?;
            let marker = path.join(CARGO_CACHE_OWNER);
            let marker_before = std::fs::read(&marker)?;
            let permit = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let permit_for_check = Arc::clone(&permit);
            let keep_going = move || permit_for_check.load(std::sync::atomic::Ordering::Relaxed);
            let scan = CacheGcScan {
                keep_going: Some(&keep_going),
                ..CacheGcScan::default()
            };
            let permit_for_event = Arc::clone(&permit);
            let event: Event = Arc::new(move |event, path| {
                if event == "unlink" && path.file_name() == Some(OsStr::new("artifact")) {
                    permit_for_event.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                Ok(())
            });
            let first = sweep_with_scan(home, &CacheGcPolicy::default(), event, &scan)?;
            let tombstone = first
                .interrupted_at
                .as_ref()
                .ok_or("partial delete tombstone")?;
            assert!(first.continuation.is_some());
            assert!(!tombstone.join("artifact").exists());
            assert_eq!(
                std::fs::read(tombstone.join(CARGO_CACHE_OWNER))?,
                marker_before
            );

            let detached = Directory::open_anchor(tombstone)?;
            assert!(matches!(
                detached.try_flock_foreign(OsStr::new(CARGO_CACHE_OWNER), LockMode::Exclusive)?,
                ForeignLock::Acquired(_)
            ));
            drop(detached);

            let resume_scan = CacheGcScan {
                start_at: Some(tombstone),
                detail: first.continuation.as_deref(),
                ..CacheGcScan::default()
            };
            let resumed = sweep_with_scan(
                home,
                &CacheGcPolicy::default(),
                Arc::new(|_, _| Ok(())),
                &resume_scan,
            )?;
            assert!(resumed.interrupted_at.is_none());
            assert!(!tombstone.exists());
            Ok(())
        }

        #[test]
        fn total_cap_evicts_only_oldest_idle_cache_and_keeps_protected_and_foreign()
        -> Result<(), Box<dyn std::error::Error>> {
            for dry_run in [true, false] {
                let temporary = tempfile::tempdir()?;
                let home = temporary.path();
                let cache_root = home.join("Library/Caches/provenant-lane-cargo");
                std::fs::create_dir_all(&cache_root)?;
                let locks = test_writer_lock(home)?;
                let mut paths = Vec::new();
                let mut owners = Vec::new();
                let mut sizes = Vec::new();
                for (letter, seconds) in [("a", 30), ("b", 10), ("c", 1), ("d", 2)] {
                    let owner = home.join(letter);
                    std::fs::create_dir(&owner)?;
                    let path = cache(home, &letter.repeat(64), &owner)?;
                    for item in [&path, &path.join(CARGO_CACHE_OWNER), &path.join("artifact")] {
                        std::fs::File::open(item)?.set_modified(
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
                        )?;
                    }
                    let directory = Directory::open_anchor(&path)?;
                    sizes.push(
                        measure_cache(&directory, Path::new("."), None, &CacheGcScan::default())?
                            .stats
                            .ok_or("complete cache measurement")?
                            .allocated_bytes,
                    );
                    paths.push(path);
                    owners.push(owner);
                }
                let total: u64 = sizes.iter().sum();
                let cap = total - sizes[1];
                let policy = CacheGcPolicy {
                    idle_before_ns: Some(20_000_000_000),
                    max_total_bytes: Some(cap),
                    dry_run,
                    project_owners: owners[..3].iter().cloned().collect(),
                    protected_owners: [(owners[2].clone(), Occupancy::Observed { process: None })]
                        .into(),
                    ..Default::default()
                };
                let root = Directory::open_anchor(&cache_root)?;
                let mut report = CacheGcReport {
                    dry_run,
                    ..CacheGcReport::default()
                };
                let scan = CacheGcScan::default();
                let mut writer_guard = |owner: &Path| {
                    assert_ne!(owner, owners[2]);
                    assert_ne!(owner, owners[3]);
                    locks
                        .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                        .map(Some)
                        .map_err(|_| missing_custody(owner))
                };
                collect_total_cap(
                    &root,
                    &cache_root,
                    &policy,
                    &mut report,
                    &mut writer_guard,
                    &scan,
                )?;
                assert_eq!(report.removed.len(), 1);
                assert_eq!(report.removed[0].path, paths[1]);
                assert_eq!(report.removed[0].reason, CacheGcReason::TotalCap);
                assert_eq!(report.total_bytes_after, Some(cap));
                assert_eq!(paths[1].exists(), dry_run);
                assert!(paths[0].exists() && paths[2].exists() && paths[3].exists());

                // The warm, protected and foreign entries make an explicit zero
                // cap unattainable; no owner guard can override those holds.
                let policy = CacheGcPolicy {
                    max_total_bytes: Some(0),
                    dry_run: false,
                    ..policy
                };
                let mut report = CacheGcReport::default();
                let scan = CacheGcScan::default();
                let mut writer_guard = |owner: &Path| Err(missing_custody(owner));
                collect_total_cap(
                    &root,
                    &cache_root,
                    &policy,
                    &mut report,
                    &mut writer_guard,
                    &scan,
                )?;
                assert!(report.removed.is_empty());
                assert!(report.total_bytes_after.is_some_and(|bytes| bytes > 0));
            }
            Ok(())
        }

        #[test]
        fn total_cap_and_free_floor_keep_recent_shared_caches()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let owner = home.join("owner");
            std::fs::create_dir(&owner)?;
            let path = cache(home, &"a".repeat(64), &owner)?;
            let recent = std::time::UNIX_EPOCH + std::time::Duration::from_secs(30);
            for item in [&path, &path.join(CARGO_CACHE_OWNER), &path.join("artifact")] {
                std::fs::File::open(item)?
                    .set_times(std::fs::FileTimes::new().set_modified(recent))?;
            }
            let locks = test_writer_lock(home)?;
            for free_floor_bytes in [None, Some(1)] {
                let policy = CacheGcPolicy {
                    idle_before_ns: Some(20_000_000_000),
                    max_total_bytes: Some(0),
                    free_floor_bytes,
                    project_owners: [owner.clone()].into(),
                    ..Default::default()
                };
                let report = gc_caches_with_free_space(
                    home,
                    &policy,
                    |owner| {
                        locks
                            .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                            .map(Some)
                            .map_err(|_| missing_custody(owner))
                    },
                    |_| Ok(0),
                )?;
                assert!(report.removed.is_empty());
                assert!(report.total_bytes_after.is_some_and(|bytes| bytes > 0));
                assert!(path.join("artifact").is_file());
            }
            Ok(())
        }

        #[test]
        fn incomplete_preview_never_authorises_idle_eviction()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let owner_path = home.join("owner");
            std::fs::create_dir(&owner_path)?;
            let path = cache(home, &"a".repeat(64), &owner_path)?;
            let directory = Directory::open_anchor(&path)?;
            let owner = read_cache_owner(&directory, Path::new("."))?.ok_or("owner")?;
            let policy = CacheGcPolicy {
                idle_before_ns: Some(i128::MAX),
                project_owners: [owner_path].into(),
                dry_run: true,
                ..Default::default()
            };
            let keep_going = || false;
            let scan = CacheGcScan {
                keep_going: Some(&keep_going),
                ..CacheGcScan::default()
            };
            let mut report = CacheGcReport::default();
            assert_eq!(
                cache_gc_reason(&directory, &path, &owner, &policy, &scan, None, &mut report,)?,
                None
            );
            assert_eq!(report.interrupted_at.as_deref(), Some(path.as_path()));
            assert!(report.continuation.is_some());
            assert!(path.join("artifact").exists());
            Ok(())
        }

        #[test]
        fn nested_tree_preview_and_apply_use_the_same_allocated_bytes()
        -> Result<(), Box<dyn std::error::Error>> {
            let home = tempfile::tempdir()?;
            let owner = home.path().join("missing-owner");
            let path = cache(home.path(), &"a".repeat(64), &owner)?;
            std::fs::create_dir_all(path.join("nested/deeper"))?;
            std::fs::write(path.join("nested/deeper/payload"), vec![0; 128 * 1024])?;
            fn whole_tree(path: &Path) -> std::io::Result<u64> {
                use std::os::unix::fs::MetadataExt;
                let metadata = std::fs::symlink_metadata(path)?;
                let mut bytes = metadata.blocks() * 512;
                if metadata.is_dir() {
                    for entry in std::fs::read_dir(path)? {
                        bytes = bytes.saturating_add(whole_tree(&entry?.path())?);
                    }
                }
                Ok(bytes)
            }
            let expected = whole_tree(&path)?;
            let preview = gc_worktree_cargo_caches(
                home.path(),
                &CacheGcPolicy {
                    dry_run: true,
                    ..Default::default()
                },
            )?;
            let apply = gc_worktree_cargo_caches(home.path(), &CacheGcPolicy::default())?;
            assert_eq!(preview.bytes_freed, expected);
            assert_eq!(preview.bytes_freed, apply.bytes_freed);
            assert!(!path.exists());
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
                // Idle at measurement; the concurrent write lands after "now".
                let now_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
                    .try_into()?;
                let policy = CacheGcPolicy {
                    idle_before_ns: Some(now_ns),
                    free_floor_bytes: Some(1),
                    project_owners: [owner].into(),
                    ..CacheGcPolicy::default()
                };
                let report = gc_caches_with_free_space(
                    home,
                    &policy,
                    |owner| {
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
                        result.map_err(|_| missing_custody(owner))?;
                        locks
                            .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                            .map(Some)
                            .map_err(|_| missing_custody(owner))
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
            let marker = encode_cache_owner(&CacheOwner {
                path: home.join("missing"),
                identity: (1, 2),
            });
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
            assert!(error.report.removed.iter().any(|entry| entry.path == first));
            assert!(error.report.removed.iter().any(|entry| entry.path == last));
            let PermissionError::CacheSweepIo(source) = &*error.source else {
                panic!("unexpected error: {error:?}");
            };
            let failures = source
                .get_ref()
                .and_then(|source| source.downcast_ref::<CacheSweepFailures>())
                .ok_or("typed sweep failures")?;
            assert!(failures.0.iter().any(|(name, failure)| {
                name.is_none()
                    && matches!(
                        failure,
                        PermissionError::CacheSweepFilesystem {
                            operation: "readdir",
                            source: FsError::Io { source, .. },
                            ..
                        } if source.kind() == io::ErrorKind::PermissionDenied
                    )
            }));
            Ok(())
        }

        #[test]
        fn lane_cache_gc_removes_large_private_payload_tree()
        -> Result<(), Box<dyn std::error::Error>> {
            let temporary = tempfile::tempdir()?;
            let home = temporary.path();
            let path = cache(home, &"a".repeat(64), &home.join("missing-worktree"))?;
            let outside = home.join("outside");
            std::fs::create_dir(&outside)?;
            std::fs::write(outside.join("keep"), b"must survive")?;
            std::os::unix::fs::symlink(&outside, path.join("outside.lock"))?;
            std::fs::create_dir(path.join("directory.lock"))?;
            let registry = path.join("private-config/packages");
            let target = path.join("target/debug");
            std::fs::create_dir_all(&target)?;
            // A real Cargo profile directory carries its lock file; GC probes it
            // instead of enumerating the profile's (unbounded) artifact entries.
            std::fs::write(target.join(".cargo-lock"), b"")?;
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
                (Occupancy::Observed { process: None }, CacheGcReason::Live),
                (Occupancy::Held, CacheGcReason::Busy),
                (
                    Occupancy::custody_unknown(owner.clone(), "fixture"),
                    CacheGcReason::Unobservable,
                ),
            ] {
                for dry_run in [true, false] {
                    let policy = CacheGcPolicy {
                        idle_before_ns: Some(i128::MAX),
                        dry_run,
                        project_owners: [owner.clone()].into(),
                        protected_owners: [(owner.clone(), state.clone())].into(),
                        selected_only: false,
                        free_floor_bytes: None,
                        max_total_bytes: None,
                    };
                    let report =
                        gc_worktree_cargo_caches_with_writer_guard(home, &policy, |path| {
                            Err(missing_custody(path))
                        })?;
                    assert!(report.removed.is_empty());
                    assert_eq!(report.kept.len(), 1);
                    assert_eq!(report.kept[0].reason, reason);
                    assert!(path.join("artifact").is_file());
                }
            }
            Ok(())
        }

        #[test]
        fn gc_idle_refuses_writers_that_became_held_or_unobservable()
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
                let report =
                    gc_worktree_cargo_caches_with_writer_guard(home, &policy, |owner_path| {
                        locks
                            .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                            .map(Some)
                            .map_err(|error| match error {
                                FsError::LockTimeout { .. } => Occupancy::Held,
                                _ => missing_custody(owner_path),
                            })
                    })?;
                assert!(report.removed.is_empty());
                assert_eq!(report.kept.len(), 1);
                assert_eq!(
                    report.kept[0].reason,
                    if live {
                        CacheGcReason::Busy
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
            use provenant_core::{AttemptId, GitOid};
            use provenant_os::clock::SystemClock;
            use provenant_os::fs::Directory;
            use provenant_records::custody::Scope;
            use provenant_supervisor::custody::{self, CustodyError, WriterBinding};
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
                &mut |path| {
                    locks
                        .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                        .map(Some)
                        .map_err(|_| missing_custody(path))
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
                            .map(Some)
                            .map_err(io::Error::other)?;
                    }
                    Ok(())
                }),
                &mut |path| {
                    probed.store(true, Ordering::SeqCst);
                    locks
                        .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                        .map(Some)
                        .map_err(|_| missing_custody(path))
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
                    &mut |path| {
                        locks
                            .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                            .map(Some)
                            .map_err(|_| missing_custody(path))
                    },
                )?;
                assert!(report.removed.is_empty());
                assert!(path.join(CARGO_CACHE_OWNER).is_file());
            }
            Ok(())
        }

        #[test]
        fn gc_refuses_owner_identity_changes_at_each_sweep_boundary()
        -> Result<(), Box<dyn std::error::Error>> {
            type Case = (&'static str, fn() -> Result<(), Box<dyn std::error::Error>>);
            let cases: [Case; 3] = [
                (
                    "before rename",
                    gc_refuses_replacement_identity_after_rename,
                ),
                ("verified open", gc_reports_swap_during_verified_open),
                ("after rename", gc_quarantines_changed_owner_after_rename),
            ];
            for (boundary, verify) in cases {
                if let Err(error) = verify() {
                    return Err(format!("{boundary}: {error}").into());
                }
            }
            Ok(())
        }

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
            let PermissionError::CacheSweepIo(source) = &*error.source else {
                panic!("unexpected error: {error:?}");
            };
            let failures = source
                .get_ref()
                .unwrap()
                .downcast_ref::<CacheSweepFailures>()
                .unwrap();
            assert!(
                matches!(
                    &failures.0[0].1,
                    PermissionError::CacheSweepFilesystem {
                        operation: "open verified directory",
                        source: FsError::RaceRefused { .. },
                        ..
                    }
                ),
                "{error:?}"
            );
            assert_eq!(error.report.bytes_freed, 0);
            Ok(())
        }

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
            let PermissionError::CacheSweepIo(source) = &*error.source else {
                panic!("unexpected error: {error:?}");
            };
            let failures = source
                .get_ref()
                .unwrap()
                .downcast_ref::<CacheSweepFailures>()
                .unwrap();
            assert!(failures.0.iter().any(|(_, failure)| matches!(
                failure,
                PermissionError::CacheSweepFilesystem {
                    operation: "unlink",
                    path,
                    source: FsError::Io { source, .. },
                } if path.file_name() == Some(OsStr::new("z-fail"))
                    && source.kind() == io::ErrorKind::PermissionDenied
            )));
            assert!(!ordinary.exists());
            let ordinary_removed = error
                .report
                .removed
                .iter()
                .find(|entry| entry.path == ordinary)
                .ok_or("successful cache progress")?;
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
                payload_bytes + ordinary_removed.bytes
            );
            let report = gc_worktree_cargo_caches(home, &CacheGcPolicy::default())?;
            assert!(
                report
                    .removed
                    .iter()
                    .any(|entry| entry.reason == CacheGcReason::Tombstone)
            );
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
            let marker = encode_cache_owner(&CacheOwner {
                path: owner.clone(),
                identity: (identity.dev(), inode),
            });
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
    fn cache_age_applies_to_orphaned_and_foreign_caches_and_busy_wins()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::MetadataExt;

        let temporary = tempfile::tempdir()?;
        let home = temporary.path();
        let foreign_owner_path = home.join("foreign");
        std::fs::create_dir(&foreign_owner_path)?;
        let foreign_identity = std::fs::metadata(&foreign_owner_path)?;
        let foreign = CacheOwner {
            path: foreign_owner_path.clone(),
            identity: (foreign_identity.dev(), foreign_identity.ino()),
        };
        let missing_owner = CacheOwner {
            path: home.join("removed"),
            identity: (1, 2),
        };
        let policy = CacheGcPolicy {
            idle_before_ns: Some(20),
            ..Default::default()
        };

        for (owner, expected_old, expected_recent) in [
            (&foreign, Some(CacheGcReason::Foreign), None),
            (&missing_owner, Some(CacheGcReason::Orphaned), None),
        ] {
            assert_eq!(
                cache_gc_reason_with_newest(Path::new("cache"), owner, &policy, || Ok(10))?,
                expected_old
            );
            assert_eq!(
                cache_gc_reason_with_newest(Path::new("cache"), owner, &policy, || Ok(30))?,
                expected_recent
            );
        }

        let mut busy_policy = policy;
        busy_policy
            .protected_owners
            .insert(missing_owner.path.clone(), Occupancy::Held);
        assert_eq!(
            cache_gc_reason_with_newest(Path::new("cache"), &missing_owner, &busy_policy, || {
                panic!("a busy cache must not measure its contents")
            })?,
            None
        );
        Ok(())
    }

    #[test]
    fn sweep_keeps_recent_orphans_and_all_foreign_caches() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temporary = tempfile::tempdir()?;
        let home = temporary.path();
        let cache_root = home.join("Library/Caches/provenant-lane-cargo");
        std::fs::create_dir_all(&cache_root)?;
        let threshold = std::time::SystemTime::now();
        let old = threshold - std::time::Duration::from_secs(3600);
        let recent = threshold + std::time::Duration::from_secs(3600);
        let mut caches = Vec::new();

        for (letter, name, timestamp) in [
            ("a", "orphan-old", old),
            ("b", "orphan-recent", recent),
            ("c", "foreign-old", old),
            ("d", "foreign-recent", recent),
        ] {
            let owner = home.join(name);
            std::fs::create_dir(&owner)?;
            let identity = std::fs::metadata(&owner)?;
            let path = cache_root.join(letter.repeat(64));
            std::fs::create_dir(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            std::fs::write(
                path.join(CARGO_CACHE_OWNER),
                encode_cache_owner(&CacheOwner {
                    path: owner.clone(),
                    identity: (identity.dev(), identity.ino()),
                }),
            )?;
            std::fs::write(path.join("artifact"), b"cached")?;
            for item in [&path, &path.join(CARGO_CACHE_OWNER), &path.join("artifact")] {
                std::fs::File::open(item)?.set_modified(timestamp)?;
            }
            if name.starts_with("orphan-") {
                std::fs::remove_dir(&owner)?;
            }
            caches.push((path, name.to_owned()));
        }

        let threshold_ns = threshold.duration_since(std::time::UNIX_EPOCH)?.as_nanos() as i128;
        let policy = CacheGcPolicy {
            idle_before_ns: Some(threshold_ns),
            ..Default::default()
        };
        let mut writer_guard = |path: &Path| Err(missing_custody(path));
        let report = gc_worktree_cargo_caches_with_writer_guard(home, &policy, &mut writer_guard)?;

        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].path, caches[0].0);
        assert_eq!(report.removed[0].reason, CacheGcReason::Orphaned);
        assert!(!caches[0].0.exists());
        assert!(
            caches[1].0.join("artifact").is_file(),
            "recent orphan survives"
        );
        assert!(
            caches[2].0.join("artifact").is_file(),
            "cold foreign cache survives"
        );
        assert!(
            caches[3].0.join("artifact").is_file(),
            "recent foreign cache survives"
        );
        Ok(())
    }

    #[test]
    fn lane_cache_gc_keeps_held_cargo_and_owner_locks() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir()?;
        let home = temporary.path();
        let root = home.join("Library/Caches/provenant-lane-cargo");
        std::fs::create_dir_all(&root)?;
        for (digit, lock_path) in [("a", "target/debug/.cargo-lock"), ("b", CARGO_CACHE_OWNER)] {
            let owner = home.join(format!("gone-{digit}"));
            std::fs::create_dir(&owner)?;
            let identity = std::fs::metadata(&owner)?;
            let cache = root.join(digit.repeat(64));
            std::fs::create_dir(&cache)?;
            std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700))?;
            std::fs::write(
                cache.join(CARGO_CACHE_OWNER),
                encode_cache_owner(&CacheOwner {
                    path: owner.clone(),
                    identity: (identity.dev(), identity.ino()),
                }),
            )?;
            let lock = cache.join(lock_path);
            std::fs::create_dir_all(lock.parent().ok_or("lock parent")?)?;
            if lock_path != CARGO_CACHE_OWNER {
                std::fs::write(&lock, b"")?;
            }
            std::fs::remove_dir(owner)?;
            let directory = Directory::open_anchor(lock.parent().ok_or("lock parent")?)?;
            let ForeignLock::Acquired(guard) = directory
                .try_flock_foreign(lock.file_name().ok_or("lock name")?, LockMode::Exclusive)?
            else {
                return Err("fixture lock unavailable".into());
            };
            let policy = CacheGcPolicy {
                idle_before_ns: Some(i128::MAX),
                ..Default::default()
            };
            let report = gc_worktree_cargo_caches(home, &policy)?;
            assert!(cache.exists());
            assert!(
                report
                    .kept
                    .iter()
                    .any(|entry| entry.path == cache && entry.reason == CacheGcReason::Busy)
            );
            drop(guard);
            let report = gc_worktree_cargo_caches(home, &policy)?;
            assert!(report.removed.iter().any(|entry| entry.path == cache));
        }
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
            let marker = encode_cache_owner(&CacheOwner {
                path: owner.clone(),
                identity: (identity.dev(), identity.ino()),
            });
            std::fs::write(cache.join(CARGO_CACHE_OWNER), marker)?;
            caches.push((cache, owner));
        }
        std::fs::remove_dir(&caches[0].1)?;
        let locks = test_writer_lock(home)?;
        let mut writer_guard = |path: &Path| {
            locks
                .try_lock_existing(
                    OsStr::new("writer.lock"),
                    provenant_os::fs::LockMode::Exclusive,
                )
                .map(Some)
                .map_err(|_| missing_custody(path))
        };
        let mut policy = CacheGcPolicy {
            idle_before_ns: Some(i128::MAX),
            dry_run: true,
            project_owners: caches.iter().map(|(_, owner)| owner.clone()).collect(),
            protected_owners: [(caches[2].1.clone(), Occupancy::Observed { process: None })].into(),
            free_floor_bytes: None,
            max_total_bytes: None,
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
