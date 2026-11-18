//! Bounded retention for host-owned Cargo target generations.
use std::{
    fs::{self, ReadDir},
    io,
    path::{Path, PathBuf},
};

use provenant_confinement::cache::TreeMeasureProgress;
use provenant_core::SweepControl;
use provenant_os::fs::{Directory, DirectoryPosition, FileKind, FsError, Metadata};
use serde::{Deserialize, Serialize};

const CURSOR_VERSION: u8 = 2;

#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
    pub busy: Vec<PathBuf>,
    pub interrupted_at: Option<PathBuf>,
    pub continuation: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Staging,
    StagingCandidate,
    Incomplete,
    IncompleteCandidate,
    Measure,
    MeasureTree,
    Select,
    CountNewer,
    CandidateTree,
    RemoveCandidate,
}

/// A directory inventory stamp deliberately excludes ctime: acquiring the
/// store lock reapplies private permissions to the generations root, changing
/// ctime without changing the entries being surveyed.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct DirectoryStamp {
    device: u64,
    inode: u64,
    modified_ns: i128,
}

impl From<Metadata> for DirectoryStamp {
    fn from(metadata: Metadata) -> Self {
        Self {
            device: metadata.device,
            inode: metadata.inode,
            modified_ns: metadata.modified_ns,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Cursor {
    version: u8,
    phase: Phase,
    root: DirectoryStamp,
    offset: DirectoryPosition,
    total: u64,
    last: Option<String>,
    best: Option<String>,
    candidate: Option<String>,
    project_key: Option<String>,
    newer: usize,
    tree_detail: Option<String>,
    candidate_bytes: u64,
    partial_bytes: u64,
    resurvey_after_remove: bool,
}

impl Cursor {
    fn new(phase: Phase, root: &Directory) -> Result<Self, FsError> {
        Ok(Self {
            version: CURSOR_VERSION,
            phase,
            root: DirectoryStamp::from(root.metadata()?),
            offset: DirectoryPosition::default(),
            total: 0,
            last: None,
            best: None,
            candidate: None,
            project_key: None,
            newer: 0,
            tree_detail: None,
            candidate_bytes: 0,
            partial_bytes: 0,
            resurvey_after_remove: false,
        })
    }
}

pub fn collect(
    state: &Path,
    max_generations: usize,
    max_bytes: u64,
    dry_run: bool,
    control: &SweepControl<'_>,
) -> Result<Report, Box<dyn std::error::Error>> {
    let store = crate::target_seed::store_path(state);
    let generations_path = store.join("generations");
    let mut report = Report::default();
    let _store_lock = match crate::target_seed::lock_store(state) {
        Ok(lock) => lock,
        Err(error) if lock_timeout(error.as_ref()) => {
            report.busy.push(store);
            return Ok(report);
        }
        Err(error) => return Err(error),
    };

    let store_dir = Directory::open_anchor(&store)?;
    let generations_dir = store_dir.open_verified_directory(std::ffi::OsStr::new("generations"))?;
    let cursor = resume_cursor(
        control,
        &store,
        &generations_path,
        &store_dir,
        &generations_dir,
    )?;
    let phase = cursor.as_ref().map(|saved| saved.phase);
    if !control.should_continue() {
        let phase = phase.unwrap_or(Phase::Staging);
        let root = if matches!(phase, Phase::Staging | Phase::StagingCandidate) {
            &store_dir
        } else {
            &generations_dir
        };
        let cursor = cursor.unwrap_or(Cursor::new(phase, root)?);
        stop(
            &mut report,
            candidate_or_root(
                if matches!(phase, Phase::Staging | Phase::StagingCandidate) {
                    &store
                } else {
                    &generations_path
                },
                &cursor,
            ),
            cursor,
        );
        return Ok(report);
    }

    let build_lock = match crate::target_seed::lock_build(state) {
        Ok(lock) => Some(lock),
        Err(error) if lock_timeout(error.as_ref()) => {
            report.busy.push(store.clone());
            None
        }
        Err(error) => return Err(error),
    };

    let measurement_phase_started = matches!(
        phase,
        Some(
            Phase::Measure
                | Phase::MeasureTree
                | Phase::Select
                | Phase::CountNewer
                | Phase::CandidateTree
                | Phase::RemoveCandidate
        )
    );
    if !measurement_phase_started {
        let staging_cursor =
            if !matches!(phase, Some(Phase::Incomplete | Phase::IncompleteCandidate))
                && build_lock.is_some()
            {
                clean_root(
                    &store,
                    &store_dir,
                    Phase::Staging,
                    Phase::StagingCandidate,
                    control,
                    dry_run,
                    &mut report,
                    cursor.as_ref().filter(|saved| {
                        matches!(saved.phase, Phase::Staging | Phase::StagingCandidate)
                    }),
                    is_stage,
                )?
            } else {
                None
            };
        if let Some(cursor) = staging_cursor {
            stop(&mut report, candidate_or_root(&store, &cursor), cursor);
            return Ok(report);
        }
        drop(build_lock);
        if let Some(cursor) = clean_root(
            &generations_path,
            &generations_dir,
            Phase::Incomplete,
            Phase::IncompleteCandidate,
            control,
            dry_run,
            &mut report,
            cursor.as_ref().filter(|saved| {
                matches!(saved.phase, Phase::Incomplete | Phase::IncompleteCandidate)
            }),
            is_incomplete,
        )? {
            stop(
                &mut report,
                candidate_or_root(&generations_path, &cursor),
                cursor,
            );
            return Ok(report);
        }
    } else {
        drop(build_lock);
    }

    let mut cursor = if matches!(
        phase,
        Some(Phase::Select | Phase::CountNewer | Phase::CandidateTree | Phase::RemoveCandidate)
    ) {
        cursor.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "missing seed GC cursor for post-measure phase",
            )
        })?
    } else {
        let measured = measure_generations(
            &generations_path,
            &generations_dir,
            control,
            &mut report,
            cursor
                .as_ref()
                .filter(|saved| matches!(saved.phase, Phase::Measure | Phase::MeasureTree)),
        )?;
        let Some(cursor) = measured else {
            return Ok(report);
        };
        cursor
    };
    evict_generations(
        &generations_path,
        &generations_dir,
        max_generations,
        max_bytes,
        dry_run,
        control,
        &mut report,
        cursor,
    )?;
    Ok(report)
}

fn lock_timeout(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        error.downcast_ref::<FsError>(),
        Some(FsError::LockTimeout { .. })
    )
}

fn stop(report: &mut Report, path: PathBuf, cursor: Cursor) {
    report.interrupted_at = Some(path);
    report.continuation = serde_json::to_string(&cursor).ok();
}

fn candidate_or_root(root: &Path, cursor: &Cursor) -> PathBuf {
    cursor
        .candidate
        .as_deref()
        .and_then(decode_name)
        .map_or_else(|| root.to_path_buf(), |name| root.join(name))
}

fn resume_cursor(
    control: &SweepControl<'_>,
    store: &Path,
    generations: &Path,
    store_dir: &Directory,
    generations_dir: &Directory,
) -> Result<Option<Cursor>, FsError> {
    let (Some(start), Some(detail)) = (control.start_at, control.detail) else {
        return Ok(None);
    };
    let Ok(cursor) = serde_json::from_str::<Cursor>(detail) else {
        return Ok(None);
    };
    if cursor.version != CURSOR_VERSION {
        return Ok(None);
    }
    let root_path = match cursor.phase {
        Phase::Staging | Phase::StagingCandidate => store,
        _ => generations,
    };
    let root_dir = match cursor.phase {
        Phase::Staging | Phase::StagingCandidate => store_dir,
        _ => generations_dir,
    };
    let current_root = DirectoryStamp::from(root_dir.metadata()?);
    let root_matches = current_root == cursor.root;
    let expected_path = if matches!(
        cursor.phase,
        Phase::StagingCandidate
            | Phase::IncompleteCandidate
            | Phase::MeasureTree
            | Phase::CandidateTree
            | Phase::RemoveCandidate
    ) {
        candidate_or_root(root_path, &cursor)
    } else {
        root_path.to_path_buf()
    };
    if start == expected_path {
        let mut cursor = cursor;
        if !root_matches {
            match cursor.phase {
                Phase::Staging | Phase::Incomplete => {
                    cursor.root = current_root;
                    cursor.offset = DirectoryPosition::default();
                }
                Phase::StagingCandidate | Phase::IncompleteCandidate => {
                    // Keep the in-flight candidate. Its directory cursor is
                    // independently validated by the tree-measure owner.
                    cursor.root = current_root;
                    cursor.offset = DirectoryPosition::default();
                }
                Phase::RemoveCandidate => {
                    // A partial removal has already changed this candidate's
                    // tree. Finish that candidate before taking a fresh census.
                    cursor.root = current_root;
                    cursor.resurvey_after_remove = true;
                }
                _ => restart_measurement(&mut cursor, current_root),
            }
        }
        Ok(Some(cursor))
    } else {
        Ok(None)
    }
}

fn restart_measurement(cursor: &mut Cursor, root: DirectoryStamp) {
    cursor.phase = Phase::Measure;
    cursor.root = root;
    cursor.offset = DirectoryPosition::default();
    cursor.total = 0;
    cursor.last = None;
    cursor.best = None;
    cursor.candidate = None;
    cursor.project_key = None;
    cursor.newer = 0;
    cursor.tree_detail = None;
    cursor.candidate_bytes = 0;
    cursor.partial_bytes = 0;
    cursor.resurvey_after_remove = false;
}

fn is_stage(_: &Directory, name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with(".staging-")
}

fn is_incomplete(root: &Directory, name: &std::ffi::OsStr) -> bool {
    let Ok(metadata) = root.metadata_at(name, false) else {
        return false;
    };
    if metadata.kind != FileKind::Directory {
        return false;
    }
    let Ok(directory) = root.open_verified_directory(name) else {
        return false;
    };
    let complete = directory
        .read_bounded(std::ffi::OsStr::new("complete"), 32)
        .is_ok_and(|bytes| bytes == b"complete\n");
    !complete || metadata.mode & 0o222 != 0
}

#[allow(clippy::too_many_arguments)]
fn clean_root(
    root_path: &Path,
    root: &Directory,
    root_phase: Phase,
    candidate_phase: Phase,
    control: &SweepControl<'_>,
    dry_run: bool,
    report: &mut Report,
    saved: Option<&Cursor>,
    predicate: impl Fn(&Directory, &std::ffi::OsStr) -> bool,
) -> Result<Option<Cursor>, Box<dyn std::error::Error>> {
    let mut cursor = saved.cloned().unwrap_or(Cursor::new(root_phase, root)?);
    if cursor.phase == candidate_phase {
        if let Some(name) = cursor.candidate.as_deref().and_then(decode_name) {
            let path = root_path.join(&name);
            let outcome = clean_candidate(&path, dry_run, control, cursor.tree_detail.as_deref())?;
            if !outcome.complete {
                cursor.tree_detail = outcome.detail;
                return Ok(Some(cursor));
            };
            report.bytes = report.bytes.saturating_add(outcome.bytes);
            report.removed.push(path);
            if !dry_run {
                cursor = Cursor::new(root_phase, root)?;
            } else {
                cursor.phase = root_phase;
                cursor.candidate = None;
                cursor.tree_detail = None;
            }
        }
        if cursor.phase == candidate_phase {
            cursor = Cursor::new(root_phase, root)?;
        }
    }
    let stamp = DirectoryStamp::from(root.metadata()?);
    if cursor.phase != root_phase || cursor.root != stamp {
        cursor = Cursor::new(root_phase, root)?;
    }
    let mut entries = root.entries()?;
    if cursor.offset != DirectoryPosition::default() {
        entries.seek(cursor.offset)?;
    }
    loop {
        if !control.should_continue() {
            cursor.offset = entries.position()?;
            cursor.root = DirectoryStamp::from(root.metadata()?);
            return Ok(Some(cursor));
        }
        let Some(name) = entries.next() else { break };
        let name = name?;
        cursor.offset = entries.position()?;
        if !predicate(root, &name) {
            continue;
        }
        let metadata = match root.metadata_at(&name, false) {
            Ok(metadata) if metadata.kind == FileKind::Directory => metadata,
            _ => continue,
        };
        let _ = metadata;
        cursor.phase = candidate_phase;
        cursor.candidate = Some(encode_name(&name));
        cursor.tree_detail = None;
        cursor.offset = entries.position()?;
        cursor.root = DirectoryStamp::from(root.metadata()?);
        let path = root_path.join(&name);
        let outcome = clean_candidate(&path, dry_run, control, None)?;
        if !outcome.complete {
            cursor.tree_detail = outcome.detail;
            return Ok(Some(cursor));
        }
        report.bytes = report.bytes.saturating_add(outcome.bytes);
        report.removed.push(path);
        if dry_run {
            cursor = Cursor::new(root_phase, root)?;
            cursor.offset = entries.position()?;
        } else {
            cursor = Cursor::new(root_phase, root)?;
            // Removing an entry invalidates the directory cookie. Restart from
            // the beginning; already removed candidates no longer appear.
            entries = root.entries()?;
        }
    }
    Ok(None)
}

struct CleanOutcome {
    complete: bool,
    bytes: u64,
    detail: Option<String>,
}

fn clean_candidate(
    path: &Path,
    dry_run: bool,
    control: &SweepControl<'_>,
    detail: Option<&str>,
) -> Result<CleanOutcome, Box<dyn std::error::Error>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CleanOutcome {
                complete: true,
                bytes: 0,
                detail: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        if dry_run {
            return Ok(CleanOutcome {
                complete: true,
                bytes: allocated(&metadata),
                detail: None,
            });
        }
        if !control.should_continue() {
            return Ok(CleanOutcome {
                complete: false,
                bytes: 0,
                detail: None,
            });
        }
        fs::remove_file(path)?;
        return Ok(CleanOutcome {
            complete: true,
            bytes: allocated(&metadata),
            detail: None,
        });
    }
    if dry_run {
        let directory = Directory::open_anchor(path)?;
        let keep_going = || control.should_continue();
        let measured = provenant_confinement::cache::measure_tree_with_progress(
            &directory,
            path,
            detail,
            &keep_going,
        )?;
        return Ok(CleanOutcome {
            complete: measured.stats.is_some(),
            bytes: measured.stats.map_or(0, |stats| stats.allocated_bytes),
            detail: measured.continuation,
        });
    }
    let mut freed = 0_u64;
    let complete = remove_tree(path, control, &mut freed)?;
    Ok(CleanOutcome {
        complete,
        bytes: freed,
        detail: None,
    })
}

fn measure_generations(
    generations_path: &Path,
    generations: &Directory,
    control: &SweepControl<'_>,
    report: &mut Report,
    saved: Option<&Cursor>,
) -> Result<Option<Cursor>, Box<dyn std::error::Error>> {
    let mut cursor = saved
        .cloned()
        .unwrap_or(Cursor::new(Phase::Measure, generations)?);
    if cursor.phase == Phase::MeasureTree {
        let Some(id) = cursor.candidate.as_deref().and_then(decode_name) else {
            return Ok(None);
        };
        if let Some((directory, _)) = generation(generations, &id) {
            let path = generations_path.join(&id);
            let keep_going = || control.should_continue();
            match provenant_confinement::cache::measure_tree_with_progress(
                &directory,
                &path,
                cursor.tree_detail.as_deref(),
                &keep_going,
            ) {
                Ok(TreeMeasureProgress {
                    stats: Some(stats), ..
                }) => {
                    cursor.total = cursor.total.saturating_add(stats.allocated_bytes);
                    cursor.phase = Phase::Measure;
                    cursor.candidate = None;
                    cursor.tree_detail = None;
                }
                Ok(progress) => {
                    cursor.tree_detail = progress.continuation;
                    stop(report, path, cursor);
                    return Ok(None);
                }
                Err(error) => {
                    return Err(error.into());
                }
            }
        }
    }
    cursor.phase = Phase::Measure;
    let stamp = DirectoryStamp::from(generations.metadata()?);
    if cursor.root != stamp {
        cursor = Cursor::new(Phase::Measure, generations)?;
    }
    let mut entries = generations.entries()?;
    if cursor.offset != DirectoryPosition::default() {
        entries.seek(cursor.offset)?;
    }
    loop {
        if !control.should_continue() {
            cursor.root = DirectoryStamp::from(generations.metadata()?);
            cursor.phase = Phase::Measure;
            stop(report, generations_path.to_path_buf(), cursor.clone());
            return Ok(None);
        }
        let Some(name) = entries.next() else { break };
        let name = name?;
        let offset_after = entries.position()?;
        let Some((directory, _project_key)) = generation(generations, &name) else {
            cursor.offset = offset_after;
            continue;
        };
        let path = generations_path.join(&name);
        let keep_going = || control.should_continue();
        match provenant_confinement::cache::measure_tree_with_progress(
            &directory,
            &path,
            None,
            &keep_going,
        ) {
            Ok(TreeMeasureProgress {
                stats: Some(stats), ..
            }) => {
                cursor.total = cursor.total.saturating_add(stats.allocated_bytes);
                cursor.offset = offset_after;
            }
            Ok(progress) => {
                cursor.phase = Phase::MeasureTree;
                cursor.offset = offset_after;
                cursor.candidate = Some(encode_name(&name));
                cursor.tree_detail = progress.continuation;
                cursor.root = stamp;
                stop(report, path, cursor);
                return Ok(None);
            }
            Err(error) => {
                return Err(error.into());
            }
        }
    }
    cursor.phase = Phase::Select;
    cursor.root = DirectoryStamp::from(generations.metadata()?);
    cursor.offset = DirectoryPosition::default();
    cursor.last = None;
    cursor.best = None;
    cursor.candidate = None;
    Ok(Some(cursor))
}

#[allow(clippy::too_many_arguments)]
fn evict_generations(
    generations_path: &Path,
    generations: &Directory,
    max_generations: usize,
    max_bytes: u64,
    dry_run: bool,
    control: &SweepControl<'_>,
    report: &mut Report,
    mut cursor: Cursor,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        if let (Phase::RemoveCandidate, Some(id)) = (
            cursor.phase,
            cursor.candidate.as_deref().and_then(decode_name),
        ) {
            let path = generations_path.join(&id);
            let mut freed = 0_u64;
            let complete = if dry_run {
                true
            } else {
                remove_tree(&path, control, &mut freed)?
            };
            report.bytes = report.bytes.saturating_add(freed);
            if !complete {
                cursor.partial_bytes = cursor.partial_bytes.saturating_add(freed);
                cursor.root = DirectoryStamp::from(generations.metadata()?);
                stop(report, path, cursor);
                return Ok(());
            }
            report.removed.push(path);
            cursor.total = cursor.total.saturating_sub(cursor.candidate_bytes);
            cursor.last = cursor.candidate.take();
            cursor.candidate_bytes = 0;
            cursor.partial_bytes = 0;
            cursor.project_key = None;
            cursor.tree_detail = None;
            cursor.phase = Phase::Select;
            cursor.offset = DirectoryPosition::default();
            cursor.best = None;
            cursor.root = DirectoryStamp::from(generations.metadata()?);
            if cursor.resurvey_after_remove {
                let fresh_root = cursor.root.clone();
                restart_measurement(&mut cursor, fresh_root);
                let Some(measured) = measure_generations(
                    generations_path,
                    generations,
                    control,
                    report,
                    Some(&cursor),
                )?
                else {
                    return Ok(());
                };
                cursor = measured;
            }
        }
        if cursor.phase == Phase::CandidateTree {
            let Some(id) = cursor.candidate.as_deref().and_then(decode_name) else {
                return Ok(());
            };
            let path = generations_path.join(&id);
            if !control.should_continue() {
                stop(report, path, cursor);
                return Ok(());
            }
            let Some((directory, _)) = generation(generations, &id) else {
                cursor.last = cursor.candidate.take();
                cursor.phase = Phase::Select;
                cursor.offset = DirectoryPosition::default();
                cursor.best = None;
                continue;
            };
            let keep_going = || control.should_continue();
            match provenant_confinement::cache::measure_tree_with_progress(
                &directory,
                &path,
                cursor.tree_detail.as_deref(),
                &keep_going,
            ) {
                Ok(TreeMeasureProgress {
                    stats: Some(stats), ..
                }) => {
                    cursor.candidate_bytes = stats.allocated_bytes;
                    cursor.tree_detail = None;
                    if dry_run {
                        report.bytes = report.bytes.saturating_add(cursor.candidate_bytes);
                        report.removed.push(path);
                        cursor.total = cursor.total.saturating_sub(cursor.candidate_bytes);
                        cursor.last = cursor.candidate.take();
                        cursor.phase = Phase::Select;
                        cursor.offset = DirectoryPosition::default();
                        cursor.best = None;
                        cursor.root = DirectoryStamp::from(generations.metadata()?);
                    } else {
                        cursor.phase = Phase::RemoveCandidate;
                    }
                    continue;
                }
                Ok(progress) => {
                    cursor.tree_detail = progress.continuation;
                    stop(report, path, cursor);
                    return Ok(());
                }
                Err(error) => {
                    return Err(error.into());
                }
            }
        }
        if cursor.phase == Phase::CountNewer {
            let Some(id) = cursor.candidate.as_deref().and_then(decode_name) else {
                return Ok(());
            };
            let Some(project_key) = cursor.project_key.as_deref() else {
                return Ok(());
            };
            let keep = max_generations.max(1);
            let stamp = DirectoryStamp::from(generations.metadata()?);
            if cursor.root != stamp {
                cursor.root = stamp;
                cursor.offset = DirectoryPosition::default();
                cursor.newer = 0;
            }
            let mut entries = generations.entries()?;
            if cursor.offset != DirectoryPosition::default() {
                entries.seek(cursor.offset)?;
            }
            while cursor.newer < keep {
                if !control.should_continue() {
                    cursor.root = DirectoryStamp::from(generations.metadata()?);
                    stop(report, generations_path.to_path_buf(), cursor);
                    return Ok(());
                }
                let Some(name) = entries.next() else { break };
                let name = name?;
                cursor.offset = entries.position()?;
                if name.as_os_str() <= id.as_os_str() {
                    continue;
                }
                if generation(generations, &name).is_some_and(|(_, key)| key == project_key) {
                    cursor.newer += 1;
                }
            }
            let evict_for_count = cursor.newer >= keep;
            let evict_for_bytes = cursor.newer > 0 && cursor.total > max_bytes;
            if evict_for_count || evict_for_bytes {
                cursor.phase = Phase::CandidateTree;
                cursor.tree_detail = None;
                cursor.offset = DirectoryPosition::default();
            } else {
                cursor.last = cursor.candidate.take();
                cursor.phase = Phase::Select;
                cursor.offset = DirectoryPosition::default();
                cursor.best = None;
                cursor.project_key = None;
                cursor.newer = 0;
            }
            continue;
        }
        cursor.phase = Phase::Select;
        let stamp = DirectoryStamp::from(generations.metadata()?);
        if cursor.root != stamp {
            cursor.root = stamp;
            cursor.offset = DirectoryPosition::default();
            cursor.best = None;
        }
        let mut entries = generations.entries()?;
        if cursor.offset != DirectoryPosition::default() {
            entries.seek(cursor.offset)?;
        }
        loop {
            if !control.should_continue() {
                cursor.root = DirectoryStamp::from(generations.metadata()?);
                stop(report, generations_path.to_path_buf(), cursor);
                return Ok(());
            }
            let Some(name) = entries.next() else { break };
            let name = name?;
            cursor.offset = entries.position()?;
            if cursor
                .last
                .as_deref()
                .and_then(decode_name)
                .is_some_and(|last| name.as_os_str() <= last.as_os_str())
            {
                continue;
            }
            if generation(generations, &name).is_none() {
                continue;
            }
            let best = cursor.best.as_deref().and_then(decode_name);
            if best
                .as_ref()
                .is_none_or(|best| name.as_os_str() < best.as_os_str())
            {
                cursor.best = Some(encode_name(&name));
            }
        }
        cursor.offset = DirectoryPosition::default();
        let Some(id) = cursor.best.take().and_then(|best| decode_name(&best)) else {
            return Ok(());
        };
        let Some((_, project_key)) = generation(generations, &id) else {
            cursor.last = Some(encode_name(&id));
            continue;
        };
        cursor.candidate = Some(encode_name(&id));
        cursor.project_key = Some(project_key);
        cursor.newer = 0;
        cursor.offset = DirectoryPosition::default();
        cursor.phase = Phase::CountNewer;
        cursor.root = DirectoryStamp::from(generations.metadata()?);
    }
}

fn generation(root: &Directory, name: &std::ffi::OsStr) -> Option<(Directory, String)> {
    let metadata = root.metadata_at(name, false).ok()?;
    if metadata.kind != FileKind::Directory
        || metadata.mode & 0o222 != 0
        || root
            .open_verified_directory(name)
            .ok()?
            .read_bounded(std::ffi::OsStr::new("complete"), 32)
            .ok()?
            .as_slice()
            != b"complete\n"
    {
        return None;
    }
    let directory = root.open_verified_directory(name).ok()?;
    let project_key = directory
        .read_bounded(std::ffi::OsStr::new("manifest.json"), 64 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<crate::target_seed::Manifest>(&bytes).ok())
        .map_or_else(String::new, |manifest| manifest.project_key);
    Some((directory, project_key))
}

#[allow(clippy::too_many_arguments)]
fn remove_tree(path: &Path, control: &SweepControl<'_>, freed: &mut u64) -> io::Result<bool> {
    struct Frame {
        path: PathBuf,
        entries: ReadDir,
        allocated: u64,
    }
    fn make_writable(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = metadata.permissions();
        #[cfg(unix)]
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(path, permissions)
    }
    use std::os::unix::fs::MetadataExt;
    let root = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    if !root.is_dir() || root.file_type().is_symlink() {
        if !control.should_continue() {
            return Ok(false);
        }
        fs::remove_file(path)?;
        *freed = freed.saturating_add(root.blocks().saturating_mul(512));
        return Ok(true);
    }
    make_writable(path, &root)?;
    let mut stack = vec![Frame {
        path: path.to_path_buf(),
        entries: fs::read_dir(path)?,
        allocated: root.blocks().saturating_mul(512),
    }];
    while !stack.is_empty() {
        if !control.should_continue() {
            return Ok(false);
        }
        let next = stack
            .last_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty removal stack"))?
            .entries
            .next();
        match next {
            Some(entry) => {
                let entry = entry?;
                let child = entry.path();
                let metadata = fs::symlink_metadata(&child)?;
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    make_writable(&child, &metadata)?;
                    stack.push(Frame {
                        path: child.clone(),
                        entries: fs::read_dir(&child)?,
                        allocated: metadata.blocks().saturating_mul(512),
                    });
                } else {
                    fs::remove_file(&child)?;
                    *freed = freed.saturating_add(metadata.blocks().saturating_mul(512));
                }
            }
            None => {
                let frame = stack.pop().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "empty removal stack")
                })?;
                fs::remove_dir(&frame.path)?;
                *freed = freed.saturating_add(frame.allocated);
            }
        }
    }
    Ok(true)
}

fn allocated(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(unix)]
fn encode_name(name: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    format!("unix:{}", provenant_core::hex(name.as_bytes()))
}

#[cfg(unix)]
fn decode_name(value: &str) -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;
    Some(std::ffi::OsString::from_vec(provenant_core::unhex(
        value.strip_prefix("unix:")?,
    )?))
}

#[cfg(not(unix))]
fn encode_name(name: &std::ffi::OsStr) -> String {
    format!("utf8:{}", name.to_string_lossy())
}

#[cfg(not(unix))]
fn decode_name(value: &str) -> Option<std::ffi::OsString> {
    Some(std::ffi::OsString::from(value.strip_prefix("utf8:")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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

    fn generation(state: &Path, id: &str, bytes: usize) -> PathBuf {
        generation_for_project(state, id, bytes, "project")
    }

    #[test]
    fn retention_obeys_count_and_bytes_and_preview() {
        let state = tempfile::tempdir().unwrap();
        let old = generation(state.path(), "001", 3);
        let middle = generation(state.path(), "002", 4);
        let newest = generation(state.path(), "003", 5);
        let cap = allocated_size(&newest).unwrap();
        let expected = allocated_size(&old).unwrap() + allocated_size(&middle).unwrap();
        let preview = collect(state.path(), 2, cap, true, &SweepControl::default()).unwrap();
        assert!(old.exists() && middle.exists() && newest.exists());
        let applied = collect(state.path(), 2, cap, false, &SweepControl::default()).unwrap();
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
        let report = collect(state.path(), 1, u64::MAX, false, &SweepControl::default()).unwrap();
        assert_eq!(report.removed, vec![old_a.clone()]);
        assert!(!old_a.exists());
        assert!(only_b.exists() && new_a.exists());

        let another = tempfile::tempdir().unwrap();
        let older = generation_for_project(another.path(), "001", 3, "project-a");
        let newer = generation_for_project(another.path(), "002", 4, "project-b");
        let cap = allocated_size(&newer).unwrap();
        let retained = collect(another.path(), 10, cap, false, &SweepControl::default()).unwrap();
        assert!(retained.removed.is_empty());
        assert!(
            older.exists() && newer.exists(),
            "each project's newest seed stays warm even over the global cap"
        );
    }

    #[test]
    fn active_clone_lock_prevents_collection_and_newest_stays_warm() {
        let state = tempfile::tempdir().unwrap();
        let generation = generation(state.path(), "001", 3);
        let guard = crate::target_seed::lock_store(state.path()).unwrap();
        assert!(
            !collect(state.path(), 0, 0, false, &SweepControl::default())
                .unwrap()
                .busy
                .is_empty()
        );
        assert!(generation.exists());
        drop(guard);
        let report = collect(state.path(), 0, 0, false, &SweepControl::default()).unwrap();
        assert!(report.removed.is_empty());
        assert!(
            generation.exists(),
            "the newest generation survives even over cap"
        );
    }

    #[test]
    fn abandoned_stages_and_incomplete_generations_are_reclaimed_with_build_custody() {
        let state = tempfile::tempdir().unwrap();
        let guard = crate::target_seed::lock_store(state.path()).unwrap();
        let build = crate::target_seed::lock_build(state.path()).unwrap();
        let stage = crate::target_seed::store_path(state.path()).join(".staging-test");
        fs::create_dir(&stage).unwrap();
        let incomplete = generation(state.path(), "001", 3);
        make_tree_writable(&incomplete).unwrap();
        let missing_marker = generation(state.path(), "002", 3);
        make_tree_writable(&missing_marker).unwrap();
        fs::remove_file(missing_marker.join("complete")).unwrap();
        drop(guard);
        collect(state.path(), 10, 100, false, &SweepControl::default()).unwrap();
        assert!(stage.exists());
        assert!(!incomplete.exists());
        assert!(!missing_marker.exists());
        drop(build);
        collect(state.path(), 10, 100, false, &SweepControl::default()).unwrap();
        assert!(!stage.exists());
    }

    #[test]
    fn budget_stop_resumes_generation_inventory_and_eviction() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let state = tempfile::tempdir().unwrap();
        let old = generation(state.path(), "001", 3);
        let middle = generation(state.path(), "002", 4);
        let newest = generation(state.path(), "003", 5);
        let checks = Arc::new(AtomicUsize::new(0));
        let observed = checks.clone();
        let keep_going = move || observed.fetch_add(1, Ordering::Relaxed) < 8;
        let stopped = collect(
            state.path(),
            2,
            u64::MAX,
            false,
            &SweepControl {
                keep_going: Some(&keep_going),
                ..Default::default()
            },
        )
        .unwrap();
        let at = stopped
            .interrupted_at
            .expect("budget exhaustion records a candidate");
        let detail = stopped
            .continuation
            .expect("budget exhaustion records resumable state");
        let resumed = collect(
            state.path(),
            2,
            u64::MAX,
            false,
            &SweepControl {
                start_at: Some(&at),
                detail: Some(&detail),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(resumed.interrupted_at.is_none());
        assert!(!old.exists() && middle.exists() && newest.exists());
    }

    #[test]
    fn changed_generation_root_preserves_tree_measurement_progress() {
        use std::cell::Cell;

        let state = tempfile::tempdir().unwrap();
        let generation = generation(state.path(), "001", 3);
        make_tree_writable(&generation).unwrap();
        let target = generation.join("target");
        fs::create_dir(&target).unwrap();
        for index in 0..400 {
            fs::write(target.join(format!("artifact-{index:04}")), b"artifact").unwrap();
        }
        fs::set_permissions(&target, fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(&generation, fs::Permissions::from_mode(0o500)).unwrap();

        let mut cursor: Option<(PathBuf, String)> = None;
        let mut tree_cursor = None;
        for _ in 0..5 {
            let checks = Cell::new(0);
            let keep_going = || {
                let count = checks.get();
                checks.set(count + 1);
                count < 100
            };
            let report = if let Some((at, detail)) = cursor.take() {
                collect(
                    state.path(),
                    2,
                    u64::MAX,
                    false,
                    &SweepControl {
                        start_at: Some(&at),
                        detail: Some(&detail),
                        keep_going: Some(&keep_going),
                        ..Default::default()
                    },
                )
                .unwrap()
            } else {
                collect(
                    state.path(),
                    2,
                    u64::MAX,
                    false,
                    &SweepControl {
                        keep_going: Some(&keep_going),
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            match (report.interrupted_at, report.continuation) {
                (Some(at), Some(detail)) if at == generation => {
                    tree_cursor = Some((at, detail));
                    break;
                }
                (Some(at), Some(detail)) => cursor = Some((at, detail)),
                _ => panic!("the bounded pass must leave a resumable cursor"),
            }
        }
        let (at, detail) = tree_cursor.expect("the large target tree must stop mid-measurement");

        // Change the generation inventory while preserving the candidate tree.
        let inventory_noise = crate::target_seed::store_path(state.path())
            .join("generations")
            .join("cursor-change");
        fs::create_dir(&inventory_noise).unwrap();

        let mut cursor = Some((at, detail));
        let mut completed = false;
        for _ in 0..20 {
            let checks = Cell::new(0);
            let keep_going = || {
                let count = checks.get();
                checks.set(count + 1);
                count < 100
            };
            let (at, detail) = cursor.take().expect("a continued pass has a cursor");
            let report = collect(
                state.path(),
                2,
                u64::MAX,
                false,
                &SweepControl {
                    start_at: Some(&at),
                    detail: Some(&detail),
                    keep_going: Some(&keep_going),
                    ..Default::default()
                },
            )
            .unwrap();
            match (report.interrupted_at, report.continuation) {
                (Some(at), Some(detail)) => cursor = Some((at, detail)),
                (None, _) => {
                    completed = true;
                    break;
                }
                _ => panic!("an interrupted pass must preserve its cursor"),
            }
        }
        assert!(
            completed,
            "tree measurement must advance after the root changes"
        );
        assert!(generation.exists());
        fs::remove_dir(inventory_noise).unwrap();
    }

    fn allocated_size(path: &Path) -> io::Result<u64> {
        let metadata = fs::symlink_metadata(path)?;
        use std::os::unix::fs::MetadataExt;
        let mut size = metadata.blocks().saturating_mul(512);
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                size = size.saturating_add(allocated_size(&entry?.path())?);
            }
        }
        Ok(size)
    }

    fn make_tree_writable(path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Ok(());
        }
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(path, permissions)?;
        for entry in fs::read_dir(path)? {
            make_tree_writable(&entry?.path())?;
        }
        Ok(())
    }
}
