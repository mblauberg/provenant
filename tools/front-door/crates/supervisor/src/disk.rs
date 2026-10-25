//! Read-only disk observations shared by the inventory owners.
use crate::fs::{Directory, FileKind, Metadata};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsStr,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize)]
pub struct DiskVolume {
    pub path: PathBuf,
    pub available_bytes: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct DiskParts {
    pub target: u64,
    pub cargo_home: u64,
    pub other: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct DiskEntry {
    pub class: String,
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leak: Option<bool>,
    pub allocated_bytes: u64,
    pub unshared_bytes: u64,
    pub newest_write_ns: i128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<DiskParts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reclaim: Option<String>,
    #[serde(skip)]
    target_contents_excluded: bool,
}

impl DiskEntry {
    pub fn target_contents_excluded(&self) -> bool {
        self.target_contents_excluded
    }
}

#[derive(Debug, Default, Serialize)]
pub struct DiskTotal {
    pub count: u64,
    pub allocated_bytes: u64,
    pub unshared_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct DiskError {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct DiskReport {
    pub schema_version: u32,
    pub method: String,
    pub volume: DiskVolume,
    pub entries: Vec<DiskEntry>,
    pub totals: BTreeMap<String, DiskTotal>,
    pub errors: Vec<DiskError>,
    // One set spans every owner, so overlapping roots and hard links contribute
    // bytes only to their first observation. Directories are included too.
    #[serde(skip)]
    seen: HashSet<(u64, u64)>,
}

impl DiskReport {
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
            seen: HashSet::new(),
        }
    }

    pub fn error(&mut self, path: impl AsRef<Path>, reason: impl ToString) {
        self.errors.push(DiskError {
            path: path.as_ref().into(),
            reason: reason.to_string(),
        });
    }

    /// Measure an existing path. No links are followed beneath the trusted
    /// parent anchor. Errors preserve partial observations in the report.
    pub fn measure(&mut self, class: &str, path: &Path) -> Option<usize> {
        self.measure_with(class, path, false)
    }

    /// Split only immediate lane children into target, cargo-home and other.
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
        self.measure_node(class, parent, name, path, false)
    }

    pub fn measure_lane_at(
        &mut self,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
    ) -> Option<usize> {
        self.measure_node("lane_cache", parent, name, path, true)
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
        self.measure_node(class, &parent, name, path, lane)
    }

    fn measure_node(
        &mut self,
        class: &str,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
        lane: bool,
    ) -> Option<usize> {
        let mut entry = DiskEntry {
            class: class.into(),
            path: path.into(),
            parts: lane.then(DiskParts::default),
            ..DiskEntry::default()
        };
        if !self.walk(parent, name, path, &mut entry, lane, 0) {
            return None;
        }
        let index = self.entries.len();
        self.entries.push(entry);
        Some(index)
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
        self.seen.insert((metadata.device, metadata.inode))
    }

    fn walk(
        &mut self,
        parent: &Directory,
        name: &OsStr,
        path: &Path,
        entry: &mut DiskEntry,
        lane: bool,
        depth: usize,
    ) -> bool {
        let metadata = match parent.metadata_at(name, false) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.error(path, error);
                return false;
            }
        };
        // Suppress duplicate allocation, retaining each owner's newest write.
        if !self.claim(entry, metadata) {
            return true;
        }
        let child = if metadata.kind == FileKind::Directory {
            match parent.open_verified_directory(name) {
                Ok(child) => Some(child),
                Err(error) => {
                    self.error(path, error);
                    return false;
                }
            }
        } else {
            None
        };
        // Cache tags are only read as metadata. A symlink marker is refused.
        let host_target = !lane
            && !(entry.class == "host_target" && entry.path == path)
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
                metadata,
                child.as_ref(),
                &mut cache,
                false,
                depth,
            );
            self.entries.push(cache);
        } else {
            self.walk_contents(
                parent,
                name,
                path,
                metadata,
                child.as_ref(),
                entry,
                lane,
                depth,
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
        metadata: Metadata,
        child: Option<&Directory>,
        entry: &mut DiskEntry,
        lane: bool,
        depth: usize,
    ) {
        let private = match parent.private_size_at(name) {
            Ok(private) => private,
            Err(error) => {
                self.error(path, error);
                None
            }
        };
        self.observe(entry, metadata, private);
        let Some(child) = child else { return };
        // An explicit traversal boundary, including targets nested in scratch.
        if name == "target" {
            entry.target_contents_excluded = true;
            return;
        }
        if depth >= 256 {
            self.error(path, "directory depth limit reached");
            return;
        }
        let names = match child
            .entries()
            .and_then(|names| names.collect::<crate::fs::Result<Vec<_>>>())
        {
            Ok(mut names) => {
                names.sort();
                names
            }
            Err(error) => {
                self.error(path, error);
                return;
            }
        };
        for name in names {
            let before = entry.unshared_bytes;
            self.walk(child, &name, &path.join(&name), entry, lane, depth + 1);
            if lane && depth == 0 {
                let bytes = entry.unshared_bytes.saturating_sub(before);
                if let Some(parts) = &mut entry.parts {
                    match name.to_str() {
                        Some("target") => parts.target = bytes,
                        Some("cargo-home") => parts.cargo_home = bytes,
                        _ => parts.other = parts.other.saturating_add(bytes),
                    }
                }
            }
        }
        if lane
            && depth == 0
            && let Some(parts) = &mut entry.parts
        {
            parts.other = entry
                .unshared_bytes
                .saturating_sub(parts.target)
                .saturating_sub(parts.cargo_home);
        }
    }

    pub fn finish(&mut self) {
        self.totals.clear();
        for entry in &self.entries {
            let total = self.totals.entry(entry.class.clone()).or_default();
            total.count += 1;
            total.allocated_bytes = total.allocated_bytes.saturating_add(entry.allocated_bytes);
            total.unshared_bytes = total.unshared_bytes.saturating_add(entry.unshared_bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        error::Error,
        fs,
        os::unix::fs::{MetadataExt, symlink},
    };

    #[test]
    fn measurement_counts_hard_links_once_and_does_not_descend_target() -> Result<(), Box<dyn Error>>
    {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("one"), vec![7; 8192])?;
        fs::hard_link(temp.path().join("one"), temp.path().join("two"))?;
        fs::create_dir(temp.path().join("target"))?;
        fs::write(temp.path().join("target/ignored"), vec![9; 16384])?;
        let mut report = DiskReport::new(temp.path().into(), 0);
        let index = report
            .measure_lane(temp.path())
            .ok_or("missing measurement")?;
        let expected = fs::symlink_metadata(temp.path())?.blocks() * 512
            + fs::symlink_metadata(temp.path().join("one"))?.blocks() * 512
            + fs::symlink_metadata(temp.path().join("target"))?.blocks() * 512;
        assert_eq!(report.entries[index].allocated_bytes, expected);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let parts = report.entries[index]
            .parts
            .as_ref()
            .ok_or("missing parts")?;
        assert_eq!(
            parts.target + parts.cargo_home + parts.other,
            report.entries[index].unshared_bytes
        );
        Ok(())
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
    fn existing_lock_observation_never_creates_a_missing_lock() -> Result<(), Box<dyn Error>> {
        use crate::fs::LockMode;
        let temp = tempfile::tempdir()?;
        let directory = Directory::open_anchor(temp.path())?;
        assert!(
            directory
                .try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)
                .is_err_and(|error| error.is_not_found())
        );
        assert!(!temp.path().join("writer.lock").exists());
        fs::write(temp.path().join("writer.lock"), b"stable")?;
        let lock = directory.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)?;
        assert!(matches!(
            directory.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive),
            Err(crate::fs::FsError::LockTimeout { .. })
        ));
        drop(lock);
        let _lock = directory.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)?;
        assert_eq!(fs::read(temp.path().join("writer.lock"))?, b"stable");
        Ok(())
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
