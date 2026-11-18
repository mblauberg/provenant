//! Read-only disk observations shared by the inventory owners.
use crate::fs::{Directory, FileKind, Metadata};
use provenant_core::{Clock, MonoTime};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

struct WalkBudget {
    cancel: Option<provenant_core::CancelToken>,
    remaining: usize,
    started: MonoTime,
    reported: bool,
    duration_micros: u64,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl std::fmt::Debug for WalkBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalkBudget")
            .field("remaining", &self.remaining)
            .field("started", &self.started)
            .field("reported", &self.reported)
            .field("duration_micros", &self.duration_micros)
            .finish_non_exhaustive()
    }
}

impl WalkBudget {
    fn new(remaining: usize, duration: Duration, clock: Arc<dyn Clock + Send + Sync>) -> Self {
        let started = clock.mono_now();
        Self {
            cancel: None,
            remaining,
            started,
            reported: false,
            duration_micros: duration.as_micros().min(u128::from(u64::MAX)) as u64,
            clock,
        }
    }

    fn exhausted(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(provenant_core::CancelToken::is_cancelled)
            || self.remaining == 0
            || self
                .clock
                .mono_now()
                .as_micros()
                .saturating_sub(self.started.as_micros())
                >= self.duration_micros
    }

    fn reached_limit(
        &mut self,
        report: &mut DiskReport,
        path: &Path,
        keep_going: &dyn Fn() -> bool,
    ) -> bool {
        if !keep_going() {
            self.remaining = 0;
            if let Some(cancel) = &self.cancel {
                cancel.cancel();
            }
        }
        if !self.exhausted() {
            return false;
        }
        if !self.reported {
            report.error_kind(
                path,
                DiskErrorKind::WalkBudgetExhausted,
                "walk budget reached",
            );
            self.reported = true;
        }
        true
    }
}

impl Default for WalkBudget {
    fn default() -> Self {
        #[cfg(test)]
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(ManualClock::default());
        #[cfg(not(test))]
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(crate::clock::SystemClock::new());
        Self::new(1_000_000, Duration::from_secs(10), clock)
    }
}

#[cfg(test)]
#[derive(Default)]
struct ManualClock(std::sync::Mutex<u64>);

#[cfg(test)]
impl ManualClock {
    fn advance(&self, micros: u64) {
        let mut now = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now = now.saturating_add(micros);
    }
}

#[cfg(test)]
impl Clock for ManualClock {
    fn wall_now(&self) -> std::result::Result<provenant_core::WallTime, provenant_core::TimeError> {
        provenant_core::WallTime::from_micros(0)
    }

    fn mono_now(&self) -> MonoTime {
        MonoTime::from_micros(
            *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn wait_until(&self, deadline: MonoTime) -> std::result::Result<(), provenant_core::TimeError> {
        let mut now = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now = (*now).max(deadline.as_micros());
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskErrorKind {
    Observation,
    WalkBudgetExhausted,
}

#[derive(Debug, Serialize)]
pub struct DiskError {
    pub path: PathBuf,
    pub kind: DiskErrorKind,
    pub reason: String,
}

/// The existing state directory, or its nearest existing ancestor. Other
/// inventoried roots may sit on different volumes.
#[derive(Debug, Serialize)]
pub struct DiskVolume {
    pub path: PathBuf,
    /// Unprivileged available space on that filesystem.
    pub available_bytes: u64,
}

/// A lane cache's immediate children: `target` and the rest.
/// The parts use unshared bytes and sum to the lane's unshared total.
#[derive(Debug, Default, Serialize)]
pub struct DiskParts {
    pub target: u64,
    pub other: u64,
}

/// One inventory row. Optional class facts are omitted when unknown or
/// inapplicable, and none of them grants removal authority.
#[derive(Debug, Default, Serialize)]
pub struct DiskEntry {
    /// `lane_cache`, `worktree`, `host_target`, `attempt_tmp`,
    /// `provider_state`, `attempt_logs`, `attempt_retained`, `child_store` or `other_state`.
    /// The last four are folded into one aggregate per class.
    pub class: String,
    /// For an aggregate, the common ancestor of the folded paths.
    pub path: PathBuf,
    /// Number of inventory rows represented by a class aggregate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    /// Lane state: `live`, `idle`, `orphaned`, `foreign`, `legacy`,
    /// `tombstone`, `refused` or `unmarked`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Worktree writer: `live`, `free` or `unknown`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writer: Option<String>,
    /// Containment in the main worktree's observed HEAD; the main worktree
    /// omits it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged: Option<bool>,
    /// For `attempt_tmp`: true when the indexed attempt is terminal, false
    /// while it runs, omitted when the index is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leak: Option<bool>,
    /// Allocated blocks (`st_blocks * 512`).
    pub allocated_bytes: u64,
    /// APFS private bytes where available, otherwise allocated blocks. Not a
    /// promise of bytes recoverable by deletion.
    pub unshared_bytes: u64,
    /// Newest observed mtime, in nanoseconds since the Unix epoch.
    pub newest_write_ns: i128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<DiskParts>,
    /// `gc` when GC may reclaim the cache; a hint that GC revalidates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reclaim: Option<String>,
}

impl DiskEntry {
    fn absorb(&mut self, other: Self) {
        self.allocated_bytes = self.allocated_bytes.saturating_add(other.allocated_bytes);
        self.unshared_bytes = self.unshared_bytes.saturating_add(other.unshared_bytes);
        self.newest_write_ns = self.newest_write_ns.max(other.newest_write_ns);
    }
}

/// Run-payload retention accounting from the last GC pass.
#[derive(Debug, Default, Serialize)]
pub struct PayloadRetention {
    /// Payload bytes that custody, age or a continuation keeps out of eviction.
    pub pinned_bytes: u64,
    /// Payload bytes evicted over the store's lifetime.
    pub reclaimed_bytes: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct DiskTotal {
    pub count: u64,
    pub allocated_bytes: u64,
    pub unshared_bytes: u64,
}

/// The `data` of `provenant disk report`: a sequence of read-only
/// observations, not an atomic filesystem snapshot. Per-path errors
/// accompany a successful partial report.
#[derive(Debug, Serialize)]
pub struct DiskReport {
    pub schema_version: u32,
    /// `apfs_private_size` when every observation had a private size,
    /// otherwise `allocated_fallback`.
    pub method: String,
    pub volume: DiskVolume,
    pub entries: Vec<DiskEntry>,
    pub totals: BTreeMap<String, DiskTotal>,
    pub errors: Vec<DiskError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<PayloadRetention>,
    // Only multiply linked non-directories need inode deduplication.
    #[serde(skip)]
    seen: HashSet<(u64, u64)>,
    #[serde(skip)]
    measured_worktrees: BTreeMap<PathBuf, (i128, bool)>,
    #[serde(skip)]
    walk_budget: WalkBudget,
}

impl DiskReport {
    /// Share an enclosing operation's cancellation across recursive measurements.
    pub fn with_cancellation(mut self, cancel: provenant_core::CancelToken) -> Self {
        self.walk_budget.cancel = Some(cancel);
        self
    }
    pub fn new(path: PathBuf, available_bytes: u64) -> Self {
        Self {
            schema_version: 1,
            method: if cfg!(target_os = "macos") {
                "apfs_private_size"
            } else {
                "allocated_fallback"
            }
            .into(),
            volume: DiskVolume {
                path,
                available_bytes,
            },
            entries: Vec::new(),
            totals: BTreeMap::new(),
            errors: Vec::new(),
            payload: None,
            seen: HashSet::new(),
            measured_worktrees: BTreeMap::new(),
            walk_budget: WalkBudget::default(),
        }
    }

    /// Whether the report's shared traversal budget has been consumed.
    pub fn traversal_exhausted(&self) -> bool {
        self.walk_budget.exhausted()
    }

    /// Bound traversal across every measurement in this report. Exhaustion
    /// preserves observed bytes as a lower bound and adds an explicit error.
    pub fn with_walk_budget(
        path: PathBuf,
        available_bytes: u64,
        max_entries: usize,
        duration: Duration,
    ) -> Self {
        let mut report = Self::new(path, available_bytes);
        report.walk_budget.remaining = max_entries;
        report.walk_budget.duration_micros = duration.as_micros().min(u128::from(u64::MAX)) as u64;
        report
    }

    pub fn error(&mut self, path: impl AsRef<Path>, reason: impl ToString) {
        self.error_kind(path, DiskErrorKind::Observation, reason);
    }

    fn error_kind(&mut self, path: impl AsRef<Path>, kind: DiskErrorKind, reason: impl ToString) {
        self.errors.push(DiskError {
            path: path.as_ref().into(),
            kind,
            reason: reason.to_string(),
        });
    }

    /// Measure an existing path. No links are followed beneath the trusted
    /// parent anchor. Errors preserve partial observations in the report.
    pub fn measure(&mut self, class: &str, path: &Path) -> Option<usize> {
        self.measure_with(class, path, false)
    }

    /// Split only immediate lane children into target and other.
    /// Parts contain the same unshared-byte observations as the entry total.
    pub fn measure_lane(&mut self, path: &Path) -> Option<usize> {
        self.measure_with("lane_cache", path, true)
    }

    /// Continue from a held inventory directory instead of resolving a
    /// previously observed parent path again.
    pub fn measure_at(
        &mut self,
        class: &str,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
    ) -> Option<usize> {
        self.measure_node(class, parent, name, path, false, &|| true)
    }

    /// Measure with an enclosing cooperative deadline checked at every entry.
    pub fn measure_at_while(
        &mut self,
        class: &str,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<usize> {
        self.measure_node(class, parent, name, path, false, keep_going)
    }

    /// Add a disjoint inventory part to a caller-owned aggregate. Tagged caches
    /// remain separate entries. No per-file report rows are retained.
    pub fn measure_into_at(
        &mut self,
        entry: &mut DiskEntry,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
    ) {
        let Some(comparison) = self.comparison_path(path) else {
            return;
        };
        let mut budget = std::mem::take(&mut self.walk_budget);
        self.walk(
            parent,
            name,
            path,
            &comparison,
            entry,
            false,
            0,
            &mut budget,
            &|| true,
        );
        self.walk_budget = budget;
    }

    /// Publish an aggregate, combining shared host-root observations.
    pub fn add_rollup(&mut self, entry: DiskEntry) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|existing| existing.class == entry.class && existing.path == entry.path)
        {
            existing.absorb(entry);
        } else {
            self.entries.push(entry);
        }
    }

    pub fn measure_lane_at(
        &mut self,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
    ) -> Option<usize> {
        self.measure_node("lane_cache", parent, name, path, true, &|| true)
    }

    fn measure_with(&mut self, class: &str, path: &Path, lane: bool) -> Option<usize> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            self.error(path, "measurement requires a named entry");
            return None;
        };
        let parent = match Directory::open_anchor(parent) {
            Ok(parent) => parent,
            Err(error) => {
                self.error(path, error);
                return None;
            }
        };
        self.measure_node(class, &parent, name, path, lane, &|| true)
    }

    fn measure_node(
        &mut self,
        class: &str,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
        lane: bool,
        keep_going: &dyn Fn() -> bool,
    ) -> Option<usize> {
        let comparison = self.comparison_path(path)?;
        let mut entry = DiskEntry {
            class: class.into(),
            path: path.into(),
            parts: lane.then(DiskParts::default),
            ..DiskEntry::default()
        };
        let mut budget = std::mem::take(&mut self.walk_budget);
        let errors_before = self.errors.len();
        let entries_before = self.entries.len();
        let observed = self.walk(
            parent,
            name,
            path,
            &comparison,
            &mut entry,
            lane,
            0,
            &mut budget,
            keep_going,
        );
        self.walk_budget = budget;
        if !observed {
            return None;
        }
        if class == "worktree" {
            // Include separately classified tagged caches in the conservative
            // timestamp inherited by later overlapping inventories.
            let newest = self.entries[entries_before..]
                .iter()
                .map(|entry| entry.newest_write_ns)
                .fold(entry.newest_write_ns, i128::max);
            self.measured_worktrees
                .insert(comparison, (newest, self.errors.len() == errors_before));
        }
        let index = self.entries.len();
        self.entries.push(entry);
        Some(index)
    }

    fn comparison_path(&mut self, path: &Path) -> Option<PathBuf> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            self.error(path, "measurement requires a named entry");
            return None;
        };
        // Resolve aliases only in the trusted parent anchor. The measured
        // entry and all descendants retain descriptor-relative no-follow I/O.
        match parent.canonicalize() {
            Ok(parent) => Some(parent.join(name)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                self.error(path, error);
                None
            }
        }
    }

    fn observe(&mut self, entry: &mut DiskEntry, metadata: Metadata, private: Option<u64>) {
        entry.allocated_bytes = entry.allocated_bytes.saturating_add(metadata.allocated);
        entry.unshared_bytes = entry.unshared_bytes.saturating_add(match private {
            Some(bytes) => bytes,
            None => {
                self.method = "allocated_fallback".into();
                metadata.allocated
            }
        });
        entry.newest_write_ns = entry.newest_write_ns.max(metadata.modified_ns);
    }

    fn claim(&mut self, entry: &mut DiskEntry, metadata: Metadata) -> bool {
        entry.newest_write_ns = entry.newest_write_ns.max(metadata.modified_ns);
        metadata.kind == FileKind::Directory
            || metadata.links <= 1
            || self.seen.insert((metadata.device, metadata.inode))
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
        comparison: &Path,
        entry: &mut DiskEntry,
        lane: bool,
        depth: usize,
        budget: &mut WalkBudget,
        keep_going: &dyn Fn() -> bool,
    ) -> bool {
        if let Some((newest, complete)) = comparison
            .ancestors()
            .find_map(|root| self.measured_worktrees.get(root))
            .copied()
        {
            entry.newest_write_ns = entry.newest_write_ns.max(newest);
            if !complete {
                self.error(path, "overlapping worktree measurement was incomplete");
            }
            return true;
        }
        if budget.reached_limit(self, path, keep_going) {
            return false;
        }
        budget.remaining -= 1;
        let metadata = match parent.metadata_at(name, false) {
            Ok(metadata) => metadata,
            Err(error) if error.is_not_found() => return false,
            Err(error) => {
                self.error(path, error);
                return false;
            }
        };
        let child = if metadata.kind == FileKind::Directory {
            match parent.open_verified_directory(name) {
                Ok(child) => Some(child),
                Err(error) if error.is_not_found() => return false,
                Err(error) => {
                    self.error(path, error);
                    return false;
                }
            }
        } else {
            None
        };
        // Cache tags are only read as metadata. A symlink marker is refused.
        let host_target = matches!(entry.class.as_str(), "worktree" | "other_state")
            && !lane
            && child.as_ref().is_some_and(|dir| {
                dir.metadata_at(OsStr::new("CACHEDIR.TAG"), false)
                    .is_ok_and(|tag| tag.kind == FileKind::Regular)
            });
        if host_target {
            let mut cache = DiskEntry {
                class: "host_target".into(),
                path: path.into(),
                ..DiskEntry::default()
            };
            self.walk_contents(
                parent,
                name,
                path,
                comparison,
                metadata,
                child.as_ref(),
                &mut cache,
                false,
                depth,
                budget,
                keep_going,
            );
            self.entries.push(cache);
        } else {
            self.walk_contents(
                parent,
                name,
                path,
                comparison,
                metadata,
                child.as_ref(),
                entry,
                lane,
                depth,
                budget,
                keep_going,
            );
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_contents(
        &mut self,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
        comparison: &Path,
        metadata: Metadata,
        child: Option<&Directory>,
        entry: &mut DiskEntry,
        lane: bool,
        depth: usize,
        budget: &mut WalkBudget,
        keep_going: &dyn Fn() -> bool,
    ) {
        let private = match parent.private_size_at(name) {
            Ok(private) => private,
            Err(error) if error.is_not_found() => return,
            Err(error) => {
                self.error(path, error);
                None
            }
        };
        // Claim only after observation: a vanished name must not suppress a
        // surviving hard link. Duplicate names still retain their newest write.
        if !self.claim(entry, metadata) {
            return;
        }
        self.observe(entry, metadata, private);
        let Some(child) = child else { return };
        if depth >= 256 {
            self.error(path, "directory depth limit reached");
            return;
        }
        let names = match child.entries() {
            Ok(names) => names,
            Err(error) => {
                self.error(path, error);
                return;
            }
        };
        for name in names {
            let name = match name {
                Ok(name) => name,
                Err(error) if error.is_not_found() => continue,
                Err(error) => {
                    self.error(path, error);
                    break;
                }
            };
            if budget.reached_limit(self, path, keep_going) {
                break;
            }
            let before = entry.unshared_bytes;
            self.walk(
                child,
                &name,
                &path.join(&name),
                &comparison.join(&name),
                entry,
                lane,
                depth + 1,
                budget,
                keep_going,
            );
            if lane && depth == 0 {
                let bytes = entry.unshared_bytes.saturating_sub(before);
                if let Some(parts) = &mut entry.parts {
                    match name.to_str() {
                        Some("target") => parts.target = bytes,
                        _ => parts.other = parts.other.saturating_add(bytes),
                    }
                }
            }
        }
        if lane
            && depth == 0
            && let Some(parts) = &mut entry.parts
        {
            parts.other = entry.unshared_bytes.saturating_sub(parts.target);
        }
    }

    pub fn finish(&mut self) {
        // Fold only after all owners have contributed, so shared host-root
        // rollups still represent one row. Actionable paths remain separate.
        let mut entries: Vec<DiskEntry> = Vec::new();
        let mut aggregates = BTreeMap::<String, usize>::new();
        for mut entry in self.entries.drain(..) {
            if matches!(
                entry.class.as_str(),
                "other_state" | "attempt_logs" | "attempt_retained" | "child_store"
            ) {
                if let Some(&index) = aggregates.get(&entry.class) {
                    let aggregate = &mut entries[index];
                    aggregate.count = Some(
                        aggregate
                            .count
                            .unwrap_or(1)
                            .saturating_add(entry.count.unwrap_or(1)),
                    );
                    while !entry.path.starts_with(&aggregate.path) {
                        if !aggregate.path.pop() {
                            break;
                        }
                    }
                    aggregate.absorb(entry);
                    continue;
                }
                entry.count = Some(entry.count.unwrap_or(1));
                aggregates.insert(entry.class.clone(), entries.len());
            }
            entries.push(entry);
        }
        self.entries = entries;
        self.totals.clear();
        for entry in &self.entries {
            let total = self.totals.entry(entry.class.clone()).or_default();
            total.count = total.count.saturating_add(entry.count.unwrap_or(1));
            total.allocated_bytes = total.allocated_bytes.saturating_add(entry.allocated_bytes);
            total.unshared_bytes = total.unshared_bytes.saturating_add(entry.unshared_bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn enclosing_cancellation_stops_all_later_measurements()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let first = fixture.path().join("first");
        let second = fixture.path().join("second");
        std::fs::write(&first, vec![1; 8192])?;
        std::fs::write(&second, vec![2; 8192])?;
        let cancel = provenant_core::CancelToken::new();
        let mut report =
            super::DiskReport::new(fixture.path().into(), 0).with_cancellation(cancel.clone());
        assert!(report.measure("sample", &first).is_some());
        let observed = report.entries[0].allocated_bytes;
        cancel.cancel();
        assert!(report.measure("sample", &second).is_none());
        assert!(report.traversal_exhausted());
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].allocated_bytes, observed);
        assert!(!report.errors.is_empty());
        assert_eq!(std::fs::read(second)?, vec![2; 8192]);
        Ok(())
    }

    // Spec: docs/specs/disk.md#bounded-sweeps
    #[test]
    fn callback_budget_reaches_recursive_measurement_without_a_timer()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let tree = fixture.path().join("tree");
        std::fs::create_dir(&tree)?;
        for n in 0..256 {
            std::fs::write(tree.join(format!("file-{n}")), vec![1; 8192])?;
        }
        let clock = super::ManualClock::default();
        let keep_going = || {
            clock.advance(1);
            provenant_core::Clock::mono_now(&clock).as_micros() < 16
        };
        let mut report = super::DiskReport::new(fixture.path().into(), 0);
        let parent = crate::fs::Directory::open_anchor(fixture.path())?;
        let index = report
            .measure_at_while(
                "sample",
                &parent,
                std::ffi::OsStr::new("tree"),
                &tree,
                &keep_going,
            )
            .ok_or("root observation")?;
        assert!(report.traversal_exhausted());
        assert!(!report.errors.is_empty());
        assert!(
            report.entries[index].allocated_bytes > 0
                && report.entries[index].allocated_bytes < 256 * 8192
        );
        assert_eq!(std::fs::read_dir(tree)?.count(), 256);
        Ok(())
    }

    #[test]
    fn measurement_budget_is_shared_across_paths() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let first = fixture.path().join("first");
        let second = fixture.path().join("second");
        std::fs::write(&first, b"one")?;
        std::fs::write(&second, b"two")?;
        let mut report = super::DiskReport::with_walk_budget(
            fixture.path().into(),
            0,
            1,
            std::time::Duration::from_secs(60),
        );
        assert!(report.measure("sample", &first).is_some());
        assert!(report.measure("sample", &second).is_none());
        assert!(report.traversal_exhausted());
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(std::fs::read(second)?, b"two");
        Ok(())
    }

    use super::*;
    use std::{
        error::Error,
        fs,
        os::unix::fs::{MetadataExt, symlink},
    };

    #[test]
    fn nested_target_contents_belong_to_temporary_entry() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        fs::create_dir_all(temp.path().join("candidate-0/target/debug"))?;
        fs::write(temp.path().join("candidate-0/target/CACHEDIR.TAG"), "cache")?;
        fs::write(
            temp.path().join("candidate-0/target/debug/payload"),
            vec![1; 16384],
        )?;
        let mut report = DiskReport::new(temp.path().into(), 0);
        let index = report
            .measure("attempt_tmp", temp.path())
            .ok_or("measurement")?;
        assert!(report.entries[index].allocated_bytes >= 16384);
        assert_eq!(report.entries.len(), 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        Ok(())
    }

    #[test]
    fn walk_budget_preserves_partial_bytes_and_reports_exhaustion() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir(&first)?;
        fs::create_dir(&second)?;
        fs::write(first.join("payload"), vec![1; 4096])?;
        fs::write(second.join("payload"), vec![2; 4096])?;
        let parent = Directory::open_anchor(temp.path())?;
        let mut report = DiskReport::new(temp.path().into(), 0);
        let mut entry = DiskEntry::default();
        let clock = Arc::new(ManualClock::default());
        let mut budget = WalkBudget::new(10, Duration::from_millis(10), clock.clone());
        assert!(report.walk(
            &parent,
            OsStr::new("first"),
            &first,
            &first,
            &mut entry,
            false,
            0,
            &mut budget,
        ));
        let partial_bytes = entry.allocated_bytes;
        assert!(partial_bytes > 0);
        clock.advance(10_000);
        assert!(!report.walk(
            &parent,
            OsStr::new("second"),
            &second,
            &second,
            &mut entry,
            false,
            0,
            &mut budget
        ));
        assert!(entry.allocated_bytes >= partial_bytes);
        assert!(
            report
                .errors
                .iter()
                .any(|error| { error.kind == DiskErrorKind::WalkBudgetExhausted })
        );
        Ok(())
    }

    #[test]
    fn deleted_directory_entry_is_benign() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("gone"), b"temporary")?;
        let directory = Directory::open_anchor(temp.path())?;
        let name = directory.entries()?.next().ok_or("entry")??;
        fs::remove_file(temp.path().join(&name))?;
        let mut report = DiskReport::new(temp.path().into(), 0);
        assert!(
            report
                .measure_at("lane_cache", &directory, &name, &temp.path().join(&name))
                .is_none()
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn vanished_name_does_not_claim_a_surviving_hard_link() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("gone");
        fs::write(&path, b"temporary")?;
        let directory = Directory::open_anchor(temp.path())?;
        let name = OsStr::new("gone");
        let mut metadata = directory.metadata_at(name, false)?;
        // Model three names without requiring link creation on the host.
        metadata.links = 3;
        fs::remove_file(&path)?;
        let mut report = DiskReport::new(temp.path().into(), 0);
        let mut entry = DiskEntry::default();
        report.walk_contents(
            &directory,
            name,
            &path,
            &path,
            metadata,
            None,
            &mut entry,
            false,
            0,
            &mut WalkBudget::default(),
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(entry.allocated_bytes, 0);
        assert!(report.claim(&mut entry, metadata));
        Ok(())
    }

    #[test]
    fn nested_worktree_is_counted_once_across_overlapping_roots() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let nested = temp.path().join(".worktrees/feature");
        fs::create_dir_all(&nested)?;
        fs::write(nested.join("payload"), vec![1; 16384])?;
        let mut parent_first = DiskReport::new(temp.path().into(), 0);
        let whole = parent_first
            .measure("worktree", temp.path())
            .map(|index| parent_first.entries[index].allocated_bytes)
            .ok_or("parent")?;
        let mut child_first = DiskReport::new(temp.path().into(), 0);
        let child_bytes = child_first
            .measure("worktree", &nested)
            .map(|index| child_first.entries[index].allocated_bytes)
            .ok_or("nested")?;
        let parent_bytes = child_first
            .measure("worktree", temp.path())
            .map(|index| child_first.entries[index].allocated_bytes)
            .ok_or("parent")?;
        assert!(child_bytes > 0);
        assert_eq!(parent_bytes + child_bytes, whole);
        assert!(parent_first.errors.is_empty());
        assert!(child_first.errors.is_empty());
        Ok(())
    }

    #[test]
    fn overlapping_worktrees_and_state_resolve_symlinked_ancestors() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let real = temp.path().join("real");
        let root = real.join("container/repo");
        let nested = root.join(".worktrees/feature");
        let state = root.join("state/tmp");
        fs::create_dir_all(&nested)?;
        fs::create_dir_all(&state)?;
        fs::write(nested.join("payload"), vec![1; 16384])?;
        fs::write(state.join("payload"), vec![2; 8192])?;
        let alias = temp.path().join("alias");
        symlink(&real, &alias)?;
        let aliased_root = alias.join("container/repo");

        let mut observations = Vec::new();
        for (first, second) in [(&root, &aliased_root), (&aliased_root, &root)] {
            let mut report = DiskReport::new(root.clone(), 0);
            let child = report
                .measure("worktree", &first.join(".worktrees/feature"))
                .ok_or("nested")?;
            let child_bytes = report.entries[child].allocated_bytes;
            let parent = report.measure("worktree", second).ok_or("parent")?;
            let parent_bytes = report.entries[parent].allocated_bytes;
            let newest = report.entries[parent].newest_write_ns;
            let inventory = report
                .measure("attempt_tmp", &first.join("state/tmp"))
                .ok_or("state")?;
            assert_eq!(report.entries[inventory].allocated_bytes, 0);
            assert_eq!(report.entries[inventory].unshared_bytes, 0);
            assert_eq!(report.entries[inventory].newest_write_ns, newest);
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            observations.push((parent_bytes, child_bytes, newest));
        }
        assert_eq!(observations[0], observations[1]);
        Ok(())
    }

    #[test]
    fn state_inside_a_worktree_keeps_first_observation_ownership() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let temporary = temp.path().join("state/attempts/a/tmp");
        fs::create_dir_all(temporary.join("target"))?;
        fs::write(temporary.join("target/CACHEDIR.TAG"), "cache")?;
        fs::write(temporary.join("target/payload"), vec![1; 16384])?;
        let mut report = DiskReport::new(temp.path().into(), 0);
        report.measure("worktree", temp.path()).ok_or("worktree")?;
        let before: u64 = report
            .entries
            .iter()
            .map(|entry| entry.allocated_bytes)
            .sum();
        let newest = report
            .entries
            .iter()
            .map(|entry| entry.newest_write_ns)
            .max()
            .ok_or("newest")?;
        let index = report
            .measure("attempt_tmp", &temporary)
            .ok_or("temporary")?;
        assert_eq!(
            report
                .entries
                .iter()
                .map(|entry| entry.allocated_bytes)
                .sum::<u64>(),
            before
        );
        assert_eq!(report.entries[index].newest_write_ns, newest);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        Ok(())
    }

    #[test]
    fn measurement_counts_hard_links_once_and_sizes_target_contents() -> Result<(), Box<dyn Error>>
    {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("one"), vec![7; 8192])?;
        fs::hard_link(temp.path().join("one"), temp.path().join("two"))?;
        fs::create_dir(temp.path().join("target"))?;
        fs::write(temp.path().join("target/payload"), vec![9; 16384])?;
        let mut report = DiskReport::new(temp.path().into(), 0);
        let index = report
            .measure_lane(temp.path())
            .ok_or("missing measurement")?;
        let expected = unique_allocated_bytes(temp.path())?;
        assert_eq!(report.entries[index].allocated_bytes, expected);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let parts = report.entries[index]
            .parts
            .as_ref()
            .ok_or("missing parts")?;
        assert_eq!(
            parts.target + parts.other,
            report.entries[index].unshared_bytes
        );
        Ok(())
    }

    fn unique_allocated_bytes(root: &Path) -> Result<u64, Box<dyn Error>> {
        let mut pending = vec![root.to_owned()];
        let mut identities = HashSet::new();
        let mut bytes = 0_u64;
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path)?;
            if !identities.insert((metadata.dev(), metadata.ino())) {
                continue;
            }
            bytes = bytes.saturating_add(metadata.blocks().saturating_mul(512));
            if metadata.file_type().is_dir() {
                for entry in fs::read_dir(path)? {
                    pending.push(entry?.path());
                }
            }
        }
        Ok(bytes)
    }

    #[test]
    fn hard_link_identity_is_counted_once_across_entries_and_devices_are_distinct() {
        let mut report = DiskReport::new(PathBuf::from("/fixture"), 0);
        let mut first = DiskEntry::default();
        let mut second = DiskEntry::default();
        let mut metadata = Metadata {
            kind: FileKind::Regular,
            device: 1,
            inode: 2,
            links: 2,
            size: 1024,
            allocated: 4096,
            modified_ns: 99,
            changed_ns: 99,
            uid: 0,
            mode: 0,
        };
        if report.claim(&mut first, metadata) {
            report.observe(&mut first, metadata, Some(2000));
        }
        if report.claim(&mut second, metadata) {
            report.observe(&mut second, metadata, Some(2000));
        }
        assert_eq!(first.allocated_bytes + second.allocated_bytes, 4096);
        assert_eq!(first.unshared_bytes + second.unshared_bytes, 2000);
        assert_eq!(second.newest_write_ns, 99);
        metadata.device = 3;
        assert!(report.claim(&mut second, metadata));
    }

    #[test]
    fn unavailable_private_size_selects_allocated_fallback() {
        let mut report = DiskReport::new(PathBuf::from("/fixture"), 0);
        report.method = "apfs_private_size".into();
        let mut entry = DiskEntry::default();
        report.observe(
            &mut entry,
            Metadata {
                kind: FileKind::Regular,
                device: 1,
                inode: 2,
                links: 1,
                size: 1024,
                allocated: 4096,
                modified_ns: 3,
                changed_ns: 3,
                uid: 0,
                mode: 0,
            },
            None,
        );
        assert_eq!(report.method, "allocated_fallback");
        assert_eq!(entry.unshared_bytes, 4096);
    }

    #[test]
    fn measurement_never_follows_links_and_separates_tagged_cache() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("private"), vec![1; 32768])?;
        symlink(outside.path(), temp.path().join("link"))?;
        fs::create_dir(temp.path().join("cache"))?;
        fs::write(
            temp.path().join("cache/CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )?;
        let mut report = DiskReport::new(temp.path().into(), 0);
        report
            .measure("worktree", temp.path())
            .ok_or("missing measurement")?;
        report.finish();
        assert_eq!(report.totals["host_target"].count, 1);
        assert_eq!(report.totals["worktree"].count, 1);
        assert!(
            report
                .entries
                .iter()
                .all(|entry| entry.path.starts_with(temp.path()))
        );
        assert!(report.errors.is_empty());
        Ok(())
    }
}
