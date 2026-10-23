//! Bounded allocation accounting and descriptor-relative private-tmp pruning.
//!
//! The OS binding awaits supervisor's safe directory API. Policy runs only over
//! owned directory handles; no path-based deletion fallback is provided.
#![allow(dead_code)] // Inert until the pending supervisor binding is integrated.
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use crate::pending::{EntryInfo, StorageFs};

pub const KEEP_FAILED_TMP_BYTES: u64 = 50 * 1024 * 1024;
pub const KEEP_ENV: &str = "PROVENANT_KEEP_ATTEMPT_TMP";
pub const TREE_MAX_DEPTH: usize = 64;
pub const TREE_MAX_ENTRIES: usize = 200_000;

/// The environment owner passes whether KEEP_ENV has a nonempty value.
pub fn keep_private_tmp(keep_requested: bool, status: &str, size: u64) -> bool {
    keep_requested
        || status == "input_required"
        || (status != "ok" && size <= KEEP_FAILED_TMP_BYTES)
}

pub(crate) fn tree_bytes(fs: &impl StorageFs, path: &Path) -> u64 {
    let Some(name) = path.file_name() else {
        return 0;
    };
    let Ok(parent) = fs.open_anchor(
        path.parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    ) else {
        return 0;
    };
    let mut budget = TREE_MAX_ENTRIES;
    walk(fs, &parent, name, 0, &mut budget, &mut HashSet::new())
}

fn walk<F: StorageFs>(
    fs: &F,
    parent: &F::Dir,
    name: &OsStr,
    depth: usize,
    budget: &mut usize,
    seen: &mut HashSet<(u64, u64)>,
) -> u64 {
    let Ok(info) = fs.stat(parent, name) else {
        return 0;
    };
    if info.symlink
        || (info.links > 1 && !info.directory && seen.contains(&(info.device, info.inode)))
    {
        return 0;
    }
    seen.insert((info.device, info.inode));
    let mut total = info.allocated;
    if !info.directory || depth >= TREE_MAX_DEPTH {
        return total;
    }
    let Ok(dir) = fs.open_child(parent, name) else {
        return total;
    };
    let Ok(entries) = fs.entries(&dir) else {
        return total;
    };
    // Read at most one entry beyond the budget, matching scandir's lazy walk.
    for child in entries {
        if *budget == 0 {
            break;
        }
        let Ok(child) = child else { break };
        *budget -= 1;
        total += walk(fs, &dir, &child, depth + 1, budget, seen);
    }
    total
}

fn verified_child<F: StorageFs>(
    fs: &F,
    parent: &F::Dir,
    name: &OsStr,
    device: u64,
) -> io::Result<F::Dir> {
    let before = fs.stat(parent, name)?;
    let dir = fs.open_child(parent, name)?;
    let after = fs.info(&dir)?;
    if before.device != after.device || before.inode != after.inode || after.device != device {
        return Err(io::Error::other("directory changed or crosses a device"));
    }
    Ok(dir)
}

fn rm_at<F: StorageFs>(
    fs: &F,
    parent: &F::Dir,
    name: &OsStr,
    device: u64,
    relative: &Path,
    left: &mut Vec<PathBuf>,
) -> io::Result<()> {
    let info = fs.stat(parent, name)?;
    if !info.directory {
        return fs.unlink(parent, name);
    }
    if info.device != device {
        left.push(relative.to_owned());
        return Ok(());
    }
    let dir = verified_child(fs, parent, name, device)?;
    let _ = fs.make_writable(&dir);
    // Python listdir snapshots the deletion entries before descending.
    let names: Vec<_> = fs.entries(&dir)?.collect::<io::Result<_>>()?;
    for child in names {
        let child_path = relative.join(&child);
        if rm_at(fs, &dir, &child, device, &child_path, left).is_err() {
            left.push(child_path);
        }
    }
    drop(dir);
    if !left
        .iter()
        .any(|item| item == relative || item.starts_with(relative))
    {
        fs.rmdir(parent, name)?;
    }
    Ok(())
}

pub(crate) fn remove_at<F: StorageFs>(fs: &F, parent: &F::Dir, name: &OsStr) -> Vec<PathBuf> {
    let mut left = Vec::new();
    let result = fs
        .info(parent)
        .and_then(|info| rm_at(fs, parent, name, info.device, Path::new(name), &mut left));
    if let Err(error) = result {
        if error.kind() != io::ErrorKind::NotFound {
            left.push(PathBuf::from(name));
        }
    }
    left
}

pub(crate) fn remove_tree(fs: &impl StorageFs, anchor: &Path, path: &Path) -> Vec<PathBuf> {
    let Ok(relative) = path.strip_prefix(anchor) else {
        return vec![path.to_owned()];
    };
    let parts: Vec<_> = relative.components().collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return vec![path.to_owned()];
    }
    let mut left = Vec::new();
    let result = (|| {
        let mut dir = fs.open_anchor(anchor)?;
        let device = fs.info(&dir)?.device;
        for part in &parts[..parts.len() - 1] {
            dir = verified_child(fs, &dir, part.as_os_str(), device)?;
        }
        let name = parts[parts.len() - 1].as_os_str();
        if !fs.stat(&dir, name)?.directory {
            left.push(path.to_owned());
            return Ok(());
        }
        rm_at(fs, &dir, name, device, path, &mut left)
    })();
    if let Err(error) = result {
        if error.kind() != io::ErrorKind::NotFound {
            left.push(path.to_owned());
        }
    }
    left
}

pub(crate) fn prune_private_tmp(
    fs: &impl StorageFs,
    run: &Path,
    attempt: &Path,
    status: &str,
    keep_requested: bool,
) -> (u64, Vec<PathBuf>) {
    let tmp = attempt.join("tmp");
    if keep_requested || status == "input_required" {
        return (0, vec![]);
    }
    let Some(name) = tmp.file_name() else {
        return (0, vec![]);
    };
    let Ok(parent) = fs.open_anchor(attempt) else {
        return (0, vec![]);
    };
    let Ok(EntryInfo {
        directory: true,
        symlink: false,
        ..
    }) = fs.stat(&parent, name)
    else {
        return (0, vec![]);
    };
    let size = tree_bytes(fs, &tmp);
    if keep_private_tmp(false, status, size) {
        return (0, vec![]);
    }
    let left = remove_tree(fs, run, &tmp);
    (if left.is_empty() { size } else { 0 }, left)
}

#[cfg(test)]
mod tests;
