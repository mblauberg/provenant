//! One-way format reset. Store headers are read as bytes, never opened by SQLite.
//! All mutations are relative to held directory capabilities and never follow links.
use provenant_os::fs::{Directory, FileKind, LockMode, Metadata, PayloadTree};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Read},
    path::{Path, PathBuf},
};

pub(crate) const BLOCKED: &str = "STATE_FORMAT_BREAK";
/// Dedicated acknowledgement status, distinct from invalid arguments (2).
pub(crate) const ACK_REQUIRED: u8 = 6;
const DROPPED: &[&str] = &[
    "run.sqlite3",
    "attempts",
    "runs",
    "children",
    "fabric/fabric.sqlite3",
    "fabric/host.sqlite3",
    "fabric/graphs",
    "work-index.sqlite3",
    "pr-contributions.sqlite3",
    "admission.sqlite3",
];
const COMPANIONS: &[&str] = &["-wal", "-shm", "-journal", ".lock"];
const KEPT: &str = "usage ledger, quota history, provider history (relocated to provider-history/format-1), session checkpoints, chair log";

pub(crate) struct Plan {
    root: Directory,
    state: PathBuf,
    receipt_format: Option<u64>,
    entries: Vec<Entry>,
    pub dropped: Vec<PathBuf>,
}
struct Entry {
    parent: Directory,
    name: OsString,
    before: Metadata,
}
fn fs<T>(result: provenant_os::fs::Result<T>) -> io::Result<T> {
    result.map_err(io::Error::other)
}
fn metadata(dir: &Directory, name: &OsStr) -> io::Result<Option<Metadata>> {
    match dir.metadata_at(name, false) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}
fn directory(dir: &Directory, name: &OsStr) -> io::Result<Option<Directory>> {
    let Some(value) = metadata(dir, name)? else {
        return Ok(None);
    };
    if value.kind == FileKind::Symlink {
        return Ok(None);
    }
    Ok(Some(fs(dir.open_verified_directory(name))?))
}

/// Version numbers of the dropped stores at the format-2 cut-over, not a
/// compatibility reader. A foreign header allows recovery after an old agent
/// recreated a store, even when a format-2 receipt has already been published.
fn foreign_header(root: &Directory, name: &str, expected: u32) -> io::Result<bool> {
    let (parent, leaf) = if let Some((parent, leaf)) = name.split_once('/') {
        let Some(parent) = directory(root, OsStr::new(parent))? else {
            return Ok(false);
        };
        (parent, leaf)
    } else {
        (fs(root.try_clone())?, name)
    };
    let Some(value) = metadata(&parent, OsStr::new(leaf))? else {
        return Ok(false);
    };
    if value.kind != FileKind::Regular {
        return Ok(true);
    }
    let file = fs(parent.open_read(OsStr::new(leaf)))?;
    let mut header = [0_u8; 64];
    let complete = match file.handoff_file().take(64).read_exact(&mut header) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => false,
        Err(error) => return Err(error),
    };
    Ok(!complete
        || &header[..16] != b"SQLite format 3\0"
        || u32::from_be_bytes([header[60], header[61], header[62], header[63]]) != expected)
}

fn foreign_store_headers(store: &Directory) -> io::Result<bool> {
    let mut foreign = false;
    for (name, version) in [
        ("run.sqlite3", 12),
        (
            "fabric/fabric.sqlite3",
            provenant_fabric_store::CURRENT_SCHEMA_VERSION as u32,
        ),
        (
            "fabric/host.sqlite3",
            provenant_fabric_store::CURRENT_SCHEMA_VERSION as u32,
        ),
        ("admission.sqlite3", 6),
        ("pr-contributions.sqlite3", 2),
    ] {
        foreign |= foreign_header(store, name, version)?;
    }
    for container in ["attempts", "runs", "children"] {
        if let Some(parent) = directory(store, OsStr::new(container))? {
            for name in fs(parent.entries())? {
                if let Some(child) = directory(&parent, &fs(name)?)? {
                    foreign |= foreign_store_headers(&child)?;
                }
            }
        }
    }
    Ok(foreign)
}

pub(crate) fn plan(state: &Path, receipt_format: Option<u64>) -> io::Result<Option<Plan>> {
    let root = match Directory::open_anchor(state) {
        Ok(root) => root,
        Err(error) if error.is_not_found() && receipt_format.is_none() => return Ok(None),
        Err(error) => return Err(io::Error::other(error)),
    };
    if metadata(&root, OsStr::new("fabric"))?.is_some_and(|m| m.kind == FileKind::Symlink) {
        return Err(io::Error::other("refusing symlinked fabric directory"));
    }
    let owner = fs(root.metadata())?;
    if owner.uid != provenant_os::fs::effective_uid() || owner.mode & 0o022 != 0 {
        return Err(io::Error::other(
            "reset requires an owned state root without group/other write access",
        ));
    }
    let older = receipt_format.is_some_and(|v| v < provenant_records::STATE_FORMAT_VERSION);
    let foreign = receipt_format == Some(provenant_records::STATE_FORMAT_VERSION)
        && foreign_store_headers(&root)?;
    let mut entries = Vec::new();
    let mut dropped = Vec::new();
    for base in DROPPED {
        for suffix in std::iter::once("").chain(COMPANIONS.iter().copied()) {
            let relative = format!("{base}{suffix}");
            let (parent, name) = if let Some((container, name)) = relative.split_once('/') {
                let Some(parent) = directory(&root, OsStr::new(container))? else {
                    continue;
                };
                (parent, name)
            } else {
                (fs(root.try_clone())?, relative.as_str())
            };
            if let Some(before) = metadata(&parent, OsStr::new(name))? {
                entries.push(Entry {
                    parent,
                    name: name.into(),
                    before,
                });
                dropped.push(PathBuf::from(relative));
            }
        }
    }
    if !older && !foreign && (receipt_format.is_some() || entries.is_empty()) {
        return Ok(None);
    }
    if receipt_format.is_none() && directory(&root, OsStr::new("install"))?.is_none() {
        return Err(io::Error::other(
            "reset requires a Provenant install directory or receipt",
        ));
    }
    // The current OS capability can retire regular owner locks, but cannot
    // unlink a lock-named symlink. Refuse before any archive move or deletion.
    for entry in &entries {
        check_removal(&entry.parent, &entry.name)?;
    }
    Ok(Some(Plan {
        root,
        state: state.into(),
        receipt_format,
        entries,
        dropped,
    }))
}

/// Only traverse the run layout, not tmp payloads. Covers the base layout,
/// run-keyed stores and recursively detached format-1 child stores.
fn attempts(
    store: &Directory,
    relative: &Path,
    found: &mut Vec<(PathBuf, Directory)>,
) -> io::Result<()> {
    if let Some(parent) = directory(store, OsStr::new("attempts"))? {
        for name in fs(parent.entries())? {
            let name = fs(name)?;
            if let Some(attempt) = directory(&parent, &name)? {
                let path = relative.join("attempts").join(&name);
                attempts(&attempt, &path, found)?;
                found.push((path, attempt));
            }
        }
    }
    for container in ["runs", "children"] {
        if let Some(parent) = directory(store, OsStr::new(container))? {
            for name in fs(parent.entries())? {
                let name = fs(name)?;
                if let Some(child) = directory(&parent, &name)? {
                    attempts(&child, &relative.join(container).join(&name), found)?;
                }
            }
        }
    }
    Ok(())
}
pub(crate) fn live_lane(plan: &Plan) -> io::Result<Option<PathBuf>> {
    let mut found = Vec::new();
    attempts(&plan.root, Path::new(""), &mut found)?;
    for (path, attempt) in found {
        match attempt.try_lock_existing(OsStr::new("custody.lock"), LockMode::Exclusive) {
            Ok(_) => {}
            Err(error) if error.is_not_found() => {}
            Err(_) => return Ok(Some(plan.state.join(path))),
        }
    }
    Ok(None)
}

pub(crate) fn describe(plan: &Plan, _state: &Path, unlanded: &[String]) -> String {
    let mut text = String::from(
        "Dropped state will be reset to format 2.\nDropped classes: run stores, attempts, children, Fabric stores and graphs, work index, PR contributions, admission.sqlite3 leases, owned macOS pv-* temporary aliases.\nPresent:\n",
    );
    for path in &plan.dropped {
        text.push_str(&format!("  {}\n", path.display()));
    }
    if plan.dropped.is_empty() {
        text.push_str("  (nothing is present)\n");
    }
    text.push_str(&format!("Kept: {KEPT}\n"));
    for line in unlanded {
        text.push_str(&format!("Unlanded commits survive in Git: {line}\n"));
    }
    text
}
fn recheck(parent: &Directory, name: &OsStr, before: Metadata) -> io::Result<bool> {
    let Some(now) = metadata(parent, name)? else {
        return Ok(false);
    };
    if !before.same_identity(&now) || before.kind != now.kind {
        return Err(io::Error::other(
            "reset entry changed identity; rerun preflight",
        ));
    }
    Ok(true)
}
fn create_path(root: &Directory, path: &Path) -> io::Result<Directory> {
    let mut dir = fs(root.try_clone())?;
    for component in path.components() {
        let name = component.as_os_str();
        if metadata(&dir, name)?.is_none() {
            fs(dir.mkdir(name, 0o700))?;
            fs(dir.sync())?;
        }
        dir = fs(dir.open_verified_directory(name))?;
    }
    Ok(dir)
}
fn check_removal(parent: &Directory, name: &OsStr) -> io::Result<()> {
    let Some(value) = metadata(parent, name)? else {
        return Ok(());
    };
    if provenant_os::fs::is_reserved_lock_name(name)
        && (value.kind != FileKind::Regular || !name.as_encoded_bytes().ends_with(b".lock"))
    {
        return Err(io::Error::other(format!(
            "unsupported lock-named entry {} requires explicit removal before reset",
            name.to_string_lossy()
        )));
    }
    if value.kind == FileKind::Directory
        && !["tmp", "provider-state", "scratch", "provider-history"]
            .iter()
            .any(|part| name == *part)
    {
        let directory = fs(parent.open_verified_directory(name))?;
        for child in fs(directory.entries())? {
            check_removal(&directory, &fs(child)?)?;
        }
    }
    Ok(())
}

fn remove_entry(parent: &Directory, name: &OsStr) -> io::Result<()> {
    let Some(before) = metadata(parent, name)? else {
        return Ok(());
    };
    if before.kind == FileKind::Directory {
        fs(parent.make_owner_accessible_at(name))?;
        let child = fs(parent.open_verified_directory(name))?;
        if name == "tmp" || name == "provider-state" || name == "scratch" {
            // Quiescence and custody checks have excluded users of these
            // discarded payloads; build-output lock names guard no live owner.
            fs(child.make_private())?;
            let payload = fs(PayloadTree::claim(parent, name, &before))?;
            remove_payload(&payload)?;
        } else {
            let mut names = fs(child.entries())?
                .map(fs)
                .collect::<io::Result<Vec<_>>>()?;
            names.sort_by_key(|name| provenant_os::fs::is_reserved_lock_name(name));
            for entry in names {
                remove_entry(&child, &entry)?;
            }
        }
        if recheck(parent, name, before)? {
            fs(parent.rmdir(name))?;
        }
    } else if recheck(parent, name, before)? {
        if provenant_os::fs::is_reserved_lock_name(name) {
            let guard = fs(parent.try_lock_existing(name, LockMode::Exclusive))?;
            fs(parent.retire_owner_lock(name, &guard))?;
        } else {
            fs(parent.unlink(name))?;
        }
    }
    fs(parent.sync())
}
fn remove_payload(payload: &PayloadTree) -> io::Result<()> {
    fs(payload.make_writable())?;
    for name in fs(payload.entries())? {
        let name = fs(name)?;
        if fs(payload.metadata_at(&name))?.kind == FileKind::Directory {
            fs(payload.directory().make_owner_accessible_at(&name))?;
            remove_payload(&fs(payload.child(&name))?)?;
            fs(payload.rmdir(&name))?;
        } else {
            fs(payload.unlink(&name))?;
        }
    }
    fs(payload.directory().sync())
}
fn remove_aliases(state: &Path, attempts: &[(PathBuf, Directory)]) -> io::Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    let tmp = fs(Directory::open_anchor(Path::new("/private/tmp")))?;
    remove_aliases_in(&tmp, state, attempts)
}
fn remove_aliases_in(
    tmp: &Directory,
    state: &Path,
    attempts: &[(PathBuf, Directory)],
) -> io::Result<()> {
    for name in fs(tmp.entries())? {
        let name = fs(name)?;
        if !name.to_string_lossy().starts_with("pv-") {
            continue;
        }
        let Some(before) = metadata(tmp, &name)? else {
            continue;
        };
        if before.kind != FileKind::Symlink || before.uid != provenant_os::fs::effective_uid() {
            continue;
        }
        let target = fs(tmp.read_link(&name))?;
        if attempts
            .iter()
            .any(|(path, _)| target == state.join(path).join("tmp"))
            && recheck(tmp, &name, before)?
        {
            fs(tmp.unlink(&name))?;
        }
    }
    fs(tmp.sync())
}
pub(crate) fn remove(plan: &Plan, quiesce: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    for entry in &plan.entries {
        recheck(&entry.parent, &entry.name, entry.before)?;
    }
    quiesce()?;
    // Agents may have created another dropped store since preflight. Replan
    // after bootout, while keeping the originally selected root identity.
    let Some(refreshed) = self::plan(&plan.state, plan.receipt_format)? else {
        return Ok(());
    };
    if !fs(plan.root.metadata())?.same_identity(&fs(refreshed.root.metadata())?) {
        return Err(io::Error::other("state root changed identity before reset"));
    }
    for entry in &plan.entries {
        recheck(&entry.parent, &entry.name, entry.before)?;
    }
    let plan = &refreshed;
    for entry in &plan.entries {
        recheck(&entry.parent, &entry.name, entry.before)?;
    }
    if live_lane(plan)?.is_some() {
        return Err(io::Error::other("lane became live before reset"));
    }
    let mut found = Vec::new();
    attempts(&plan.root, Path::new(""), &mut found)?;
    // Move every archive before removing any dropped class. No replacement or
    // merge: an existing destination is ambiguous and must leave source intact.
    for (path, attempt) in &found {
        let name = OsStr::new("provider-history");
        if let Some(before) = metadata(attempt, name)? {
            if before.kind != FileKind::Directory {
                return Err(io::Error::other("provider history is not a directory"));
            }
            let archive = create_path(
                &plan.root,
                &Path::new("provider-history/format-1").join(path),
            )?;
            recheck(attempt, name, before)?;
            fs(attempt.rename_noreplace(name, &archive, name))?;
            fs(archive.sync())?;
            fs(attempt.sync())?;
        }
    }
    remove_aliases(&plan.state, &found)?;
    for entry in &plan.entries {
        remove_entry(&entry.parent, &entry.name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn refused_agent_quiescence_leaves_stores_intact()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use super::*;
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("install"))?;
        std::fs::write(temp.path().join("run.sqlite3"), "old store")?;
        let reset = plan(temp.path(), Some(1))?.ok_or("reset plan")?;
        assert!(remove(&reset, || Err(io::Error::other("agent still running"))).is_err());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("run.sqlite3"))?,
            "old store"
        );
        Ok(())
    }

    #[test]
    fn partial_removal_and_disappearing_companions_are_rerunnable()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use super::*;
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("install"))?;
        for name in ["run.sqlite3", "run.sqlite3-wal", "admission.sqlite3"] {
            std::fs::write(temp.path().join(name), "old store")?;
        }
        let reset = plan(temp.path(), Some(1))?.ok_or("reset plan")?;
        std::fs::remove_file(temp.path().join("run.sqlite3-wal"))?;
        remove(&reset, || Ok(()))?;
        remove(&reset, || Ok(()))?;
        assert!(!temp.path().join("run.sqlite3").exists());
        assert!(!temp.path().join("admission.sqlite3").exists());
        Ok(())
    }

    #[test]
    fn stores_recreated_while_stopping_agents_are_included()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use super::*;
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("install"))?;
        std::fs::write(temp.path().join("run.sqlite3"), "old store")?;
        let reset = plan(temp.path(), Some(1))?.ok_or("reset plan")?;
        remove(&reset, || {
            std::fs::write(temp.path().join("admission.sqlite3"), "recreated")
        })?;
        assert!(!temp.path().join("admission.sqlite3").exists());
        Ok(())
    }
    #[test]
    fn temporary_alias_cleanup_never_follows_or_removes_foreign_targets()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use super::*;
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let attempt = state.join("attempts/a1");
        std::fs::create_dir_all(attempt.join("tmp"))?;
        let tmp = temp.path().join("aliases");
        std::fs::create_dir(&tmp)?;
        std::os::unix::fs::symlink(attempt.join("tmp"), tmp.join("pv-owned"))?;
        std::os::unix::fs::symlink(temp.path(), tmp.join("pv-foreign"))?;
        std::fs::write(tmp.join("pv-file"), "keep")?;
        remove_aliases_in(
            &Directory::open_anchor(&tmp)?,
            &state,
            &[(
                PathBuf::from("attempts/a1"),
                Directory::open_anchor(&attempt)?,
            )],
        )?;
        assert!(std::fs::symlink_metadata(tmp.join("pv-owned")).is_err());
        assert!(std::fs::symlink_metadata(tmp.join("pv-foreign")).is_ok());
        assert_eq!(std::fs::read_to_string(tmp.join("pv-file"))?, "keep");
        assert!(attempt.join("tmp").is_dir());
        Ok(())
    }
}
