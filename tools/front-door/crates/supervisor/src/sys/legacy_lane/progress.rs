#![forbid(unsafe_code)]

use super::files;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, ReadDir};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub struct Progress {
    root: PathBuf,
    excludes: Vec<PathBuf>,
    stack: Vec<ReadDir>,
    known: BTreeMap<PathBuf, (i64, i64, u64)>,
    seen: BTreeSet<PathBuf>,
    started: SystemTime,
    initial: bool,
    pub complete: bool,
    pub latest: Option<String>,
}
impl Progress {
    pub fn new(root: PathBuf, excludes: Vec<String>, attempt: &Path) -> Self {
        let mut excludes: Vec<_> = excludes
            .into_iter()
            .map(PathBuf::from)
            .map(|p| if p.is_absolute() { p } else { root.join(p) })
            .collect();
        excludes.push(attempt.to_path_buf());
        Self {
            root,
            excludes,
            stack: Vec::new(),
            known: BTreeMap::new(),
            seen: BTreeSet::new(),
            started: SystemTime::now(),
            initial: false,
            complete: false,
            latest: None,
        }
    }
    pub fn probe(&mut self) -> bool {
        let began = Instant::now();
        let mut changed = false;
        let mut visited = 0;
        if self.stack.is_empty() {
            self.seen.clear();
            self.complete = true;
            match fs::read_dir(&self.root) {
                Ok(d) => self.stack.push(d),
                Err(_) => {
                    self.complete = false;
                    return false;
                }
            }
        }
        while !self.stack.is_empty()
            && visited < 2000
            && (visited < 32 || began.elapsed() < Duration::from_millis(25))
        {
            let entry = match self.stack.last_mut().unwrap().next() {
                None => {
                    self.stack.pop();
                    continue;
                }
                Some(Err(_)) => {
                    self.complete = false;
                    continue;
                }
                Some(Ok(e)) => e,
            };
            visited += 1;
            let path = entry.path();
            if self.excludes.iter().any(|p| path.starts_with(p)) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                self.complete = false;
                continue;
            };
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                if matches!(
                    entry.file_name().to_str(),
                    Some(
                        ".git"
                            | ".agent-run"
                            | ".worktrees"
                            | "node_modules"
                            | ".venv"
                            | "__pycache__"
                            | ".ssh"
                            | ".aws"
                            | ".kube"
                            | ".secrets"
                            | "Archives"
                            | "Monero"
                    )
                ) {
                    continue;
                }
                match fs::read_dir(path) {
                    Ok(d) => self.stack.push(d),
                    Err(_) => self.complete = false,
                };
                continue;
            }
            let signature = (meta.mtime(), meta.mtime_nsec(), meta.len());
            if self.known.get(&path).is_some_and(|s| *s != signature)
                || (!self.known.contains_key(&path)
                    && (self.initial || meta.modified().is_ok_and(|t| t > self.started)))
            {
                changed = true;
            }
            self.known.insert(path.clone(), signature);
            self.seen.insert(path);
            let stamp =
                files::timestamp_epoch(meta.mtime(), (meta.mtime_nsec() / 1_000_000) as u32);
            if self.latest.as_ref().is_none_or(|s| s < &stamp) {
                self.latest = Some(stamp);
            }
        }
        if self.stack.is_empty() {
            if self.initial && self.complete && self.known.keys().any(|p| !self.seen.contains(p)) {
                changed = true;
                self.known.retain(|p, _| self.seen.contains(p));
            }
            self.initial = true;
        }
        changed
    }
    pub fn scan_complete(&self) -> bool {
        self.stack.is_empty() && self.complete
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writer_scan_observes_changes_and_deletions_but_not_excluded_trees() {
        let root = std::env::temp_dir().join(format!("lane-progress-{}", std::process::id()));
        fs::create_dir_all(root.join("ignored")).unwrap();
        fs::create_dir_all(root.join("capture")).unwrap();
        fs::write(root.join("file"), b"initial").unwrap();
        let mut p = Progress::new(root.clone(), vec!["ignored".into()], &root.join("capture"));
        assert!(!p.probe());
        assert!(p.scan_complete());
        fs::write(root.join("ignored/changed"), b"ignored").unwrap();
        fs::write(root.join("capture/events"), b"capture").unwrap();
        assert!(!p.probe());
        fs::write(root.join("file"), b"a new version").unwrap();
        assert!(p.probe());
        fs::remove_file(root.join("file")).unwrap();
        assert!(p.probe());
        fs::remove_dir_all(root).unwrap();
    }
}
