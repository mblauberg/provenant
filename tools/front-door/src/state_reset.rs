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

/// Versions shipped by 1.x releases. This is reset evidence, not a compatibility
/// reader. Never extend these sets for a format-2 schema change.
/// Audited v1.0.0..v1.11.1 tags: run schema_v{3,4,6,7,8,9,10}.sql,
/// fabric-store db.rs migration-array lengths {7,13,15}, ledger.rs {1,2,5,6},
/// and pr_contribution.rs (introduced in v1.3.0) version 4. Intermediate
/// migration versions that were never a release's current schema are excluded.
const FORMAT_ONE_VERSIONS: &[(&str, &[u32], u32)] = &[
    (
        "run.sqlite3",
        &[3, 4, 6, 7, 8, 9, 10],
        provenant_run::RUN_SCHEMA_VERSION,
    ),
    (
        "fabric/fabric.sqlite3",
        &[7, 13, 15],
        provenant_fabric_store::CURRENT_SCHEMA_VERSION as u32,
    ),
    // The host-only file first shipped at v1.3.0 (schema 13).
    (
        "fabric/host.sqlite3",
        &[13, 15],
        provenant_fabric_store::CURRENT_SCHEMA_VERSION as u32,
    ),
    (
        "admission.sqlite3",
        &[1, 2, 5, 6],
        provenant_run::ADMISSION_SCHEMA_VERSION,
    ),
    (
        "pr-contributions.sqlite3",
        &[4],
        provenant_run::CONTRIBUTIONS_SCHEMA_VERSION,
    ),
];

fn format_one_header(
    root: &Directory,
    name: &str,
    shipped: &[u32],
    current: u32,
) -> io::Result<bool> {
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
    // Links are never evidence. An independently established reset may unlink
    // them without following them, just like other dropped links.
    if value.kind == FileKind::Symlink {
        return Ok(false);
    }
    let inspect = || -> io::Result<bool> {
        if value.kind != FileKind::Regular {
            return Err(io::Error::other("not a regular SQLite file"));
        }
        let file = fs(parent.open_read(OsStr::new(leaf)))?;
        let mut header = [0_u8; 64];
        let complete = match file.take(64).read_exact(&mut header) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => false,
            Err(error) => return Err(error),
        };
        let version = u32::from_be_bytes(header[60..64].try_into().expect("four bytes"));
        if !complete || (&header[..16] == b"SQLite format 3\0" && version == 0) {
            for suffix in ["-wal", "-shm"] {
                if metadata(&parent, OsStr::new(&format!("{leaf}{suffix}")))?.is_some() {
                    // SQLite may not have checkpointed the schema yet. Never
                    // open it or infer format 1 from an incomplete main file.
                    return Ok(false);
                }
            }
        }
        if !complete || &header[..16] != b"SQLite format 3\0" {
            return Err(io::Error::other("short or unrecognised SQLite header"));
        }
        if shipped.contains(&version) {
            return Ok(true);
        }
        // Older format-2 schemas stay non-evidence after a later 2.x bump.
        if version >= current || shipped.iter().all(|old| version > *old) {
            return Ok(false);
        }
        Err(io::Error::other(format!(
            "unknown SQLite user_version {version}"
        )))
    };
    inspect().map_err(|error| io::Error::other(format!(
        "cannot identify {}: {error}; restore this store from a known backup or repair the install receipt and store before retrying; no state was reset",
        parent.anchored_path().join(leaf).display()
    )))
}

fn foreign_store_headers(store: &Directory) -> io::Result<bool> {
    // Work index has no format-2 schema or writer. Any remaining file (even
    // only a companion after interruption) belongs to the retired class.
    let mut foreign = false;
    for suffix in std::iter::once("").chain(COMPANIONS.iter().copied()) {
        foreign |= metadata(store, OsStr::new(&format!("work-index.sqlite3{suffix}")))?.is_some();
    }
    for &(name, shipped, current) in FORMAT_ONE_VERSIONS {
        foreign |= format_one_header(store, name, shipped, current)?;
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
    let older = receipt_format == Some(1);
    // A missing receipt uses exactly the same positive evidence. Validate all
    // headers even with an old/pending receipt; corrupt files are never guessed.
    let foreign = foreign_store_headers(&root)?;
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
    if !older && !foreign {
        return Ok(None);
    }
    if receipt_format.is_none() && directory(&root, OsStr::new("install"))?.is_none() {
        return Err(io::Error::other(
            "reset requires a Provenant install directory or receipt",
        ));
    }
    // Refuse unsupported entries before any archive move or deletion. Regular
    // owner locks require custody; observed symlinks are unlinked without following.
    for entry in &entries {
        check_removal(&entry.parent, &entry.name)?;
    }
    check_provider_history(&root)?;
    Ok(Some(Plan {
        root,
        state: state.into(),
        receipt_format,
        entries,
        dropped,
    }))
}

fn check_provider_history(root: &Directory) -> io::Result<()> {
    let mut found = Vec::new();
    attempts(root, Path::new(""), &mut found)?;
    for (path, attempt) in found {
        if metadata(&attempt, OsStr::new("provider-history"))?
            .is_some_and(|value| value.kind != FileKind::Directory)
        {
            return Err(io::Error::other(format!(
                "{} is not a directory; move it aside or restore the provider history before retrying reset",
                path.join("provider-history").display()
            )));
        }
    }
    check_archive_ancestors(root, Path::new("provider-history/format-1"))?;
    let mut found = Vec::new();
    attempts(root, Path::new(""), &mut found)?;
    for (path, _) in found {
        check_archive_ancestors(root, &Path::new("provider-history/format-1").join(path))?;
    }
    Ok(())
}

fn check_archive_ancestors(root: &Directory, path: &Path) -> io::Result<()> {
    let mut dir = fs(root.try_clone())?;
    let mut relative = PathBuf::new();
    for part in path.components() {
        let name = part.as_os_str();
        relative.push(name);
        let Some(value) = metadata(&dir, name)? else {
            break;
        };
        if value.kind != FileKind::Directory {
            return Err(io::Error::other(format!(
                "{} is not a directory; move it aside or restore provider history before retrying reset",
                relative.display()
            )));
        }
        dir = fs(dir.open_verified_directory(name))?;
    }
    Ok(())
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
        && value.kind != FileKind::Symlink
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
    } else if before.kind == FileKind::Symlink {
        fs(parent.unlink_symlink(name, &before))?;
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
    quiesce()?;
    // Agents may have created another dropped store since preflight. Replan
    // after bootout, while keeping the originally selected root identity.
    let Some(refreshed) = self::plan(&plan.state, plan.receipt_format)? else {
        return Ok(());
    };
    if !fs(plan.root.metadata())?.same_identity(&fs(refreshed.root.metadata())?) {
        return Err(io::Error::other("state root changed identity before reset"));
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
    use super::*;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn header(version: u32) -> Vec<u8> {
        let mut bytes = vec![0; 64];
        bytes[..16].copy_from_slice(b"SQLite format 3\0");
        bytes[60..64].copy_from_slice(&version.to_be_bytes());
        bytes
    }

    #[test]
    fn only_shipped_format_one_versions_are_reset_evidence() -> TestResult {
        for &(name, shipped, current) in FORMAT_ONE_VERSIONS {
            assert!(
                shipped.iter().all(|version| *version < current),
                "{name}: format collision"
            );
            for version in shipped.iter().copied().chain([current, current + 1]) {
                for receipt in [None, Some(provenant_records::STATE_FORMAT_VERSION)] {
                    let temp = tempfile::tempdir()?;
                    std::fs::create_dir(temp.path().join("install"))?;
                    let path = temp.path().join(name);
                    std::fs::create_dir_all(path.parent().ok_or("store parent")?)?;
                    let bytes = header(version);
                    std::fs::write(&path, &bytes)?;
                    assert_eq!(
                        plan(temp.path(), receipt)?.is_some(),
                        shipped.contains(&version),
                        "{name}: {version}, {receipt:?}"
                    );
                    assert_eq!(std::fs::read(&path)?, bytes);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn healthy_format_two_openers_never_plan_a_reset_even_with_uncheckpointed_wal() -> TestResult {
        use provenant_core::{ActorId, AttemptId, CancelToken, ProjectId, RootInputs};
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        std::fs::create_dir(base.join("home"))?;
        std::fs::create_dir(base.join("checkout"))?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let state = roots.state.as_path();
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let context = provenant_run::ExecutionContext {
            state_root: roots.state.clone(),
            project_id: ProjectId::from_bytes([1; 16]),
            actor_id: ActorId::from_bytes([2; 16]),
            read_prompts: true,
        };
        let run = provenant_run::RunStore::open(&context, &call)?;
        // Opens the real admission ledger before looking up the absent attempt.
        assert!(
            run.set_child_wait(AttemptId::from_bytes([3; 16]), true, &call)
                .is_err()
        );
        assert!(state.join("admission.sqlite3").exists());
        let contributions = provenant_run::Contributions::open(state, &call)?;
        let fabric_call = provenant_fabric_store::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let fabric = provenant_fabric_store::Store::open(&roots.state, &fabric_call)?;
        let host = provenant_fabric_store::HostStore::open(&roots.state, &fabric_call)?;
        assert!(state.join("run.sqlite3-wal").exists());
        let run_header = std::fs::read(state.join("run.sqlite3"))?;
        assert!(
            run_header.len() < 64 || run_header[60..64] == [0; 4],
            "fixture must retain schema in WAL"
        );
        for receipt in [None, Some(provenant_records::STATE_FORMAT_VERSION)] {
            assert!(plan(state, receipt)?.is_none());
        }
        drop((run, contributions, fabric, host));
        for receipt in [None, Some(provenant_records::STATE_FORMAT_VERSION)] {
            assert!(plan(state, receipt)?.is_none());
        }
        Ok(())
    }

    #[test]
    fn unknown_headers_refuse_and_incomplete_wal_headers_are_not_reset_evidence() -> TestResult {
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("install"))?;
        let path = temp.path().join("run.sqlite3");
        for bytes in [b"not a database".to_vec(), header(0), header(5)] {
            std::fs::write(&path, &bytes)?;
            for receipt in [None, Some(1), Some(provenant_records::STATE_FORMAT_VERSION)] {
                let error = plan(temp.path(), receipt)
                    .err()
                    .ok_or("unknown header accepted")?
                    .to_string();
                assert!(error.contains("run.sqlite3") && error.contains("before retrying"));
                assert_eq!(std::fs::read(&path)?, bytes);
            }
        }
        for suffix in ["-wal", "-shm"] {
            let companion = temp.path().join(format!("run.sqlite3{suffix}"));
            std::fs::write(&companion, "uncheckpointed")?;
            for bytes in [Vec::new(), vec![0; 12], header(0)] {
                std::fs::write(&path, &bytes)?;
                for receipt in [None, Some(provenant_records::STATE_FORMAT_VERSION)] {
                    assert!(plan(temp.path(), receipt)?.is_none());
                    assert_eq!(std::fs::read(&path)?, bytes);
                }
            }
            std::fs::remove_file(companion)?;
        }
        Ok(())
    }

    #[test]
    fn refused_agent_quiescence_leaves_stores_intact()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use super::*;
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("install"))?;
        std::fs::write(temp.path().join("run.sqlite3"), header(10))?;
        let reset = plan(temp.path(), Some(1))?.ok_or("reset plan")?;
        assert!(remove(&reset, || Err(io::Error::other("agent still running"))).is_err());
        assert_eq!(std::fs::read(temp.path().join("run.sqlite3"))?, header(10));
        Ok(())
    }

    #[test]
    fn partial_removal_and_disappearing_companions_are_rerunnable()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use super::*;
        let temp = tempfile::tempdir()?;
        std::fs::create_dir(temp.path().join("install"))?;
        std::fs::write(temp.path().join("run.sqlite3"), header(10))?;
        std::fs::write(temp.path().join("run.sqlite3-wal"), "companion")?;
        std::fs::write(temp.path().join("admission.sqlite3"), header(6))?;
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
        std::fs::write(temp.path().join("run.sqlite3"), header(10))?;
        let reset = plan(temp.path(), Some(1))?.ok_or("reset plan")?;
        remove(&reset, || {
            std::fs::write(temp.path().join("admission.sqlite3"), header(6))
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
