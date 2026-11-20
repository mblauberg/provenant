//! Bounded, advisory counts of uncommitted work in a linked worktree.
//!
//! The numbers here only inform reports such as stranded work. They never
//! grant or refuse removal authority; the ordinary cleanliness verdict still
//! decides whether a checkout may be collected.
use super::*;
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::time::UNIX_EPOCH;

/// One worktree's uncommitted edits, counted for reporting only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EditStats {
    /// Distinct non-ignored paths reported by Git.
    pub files: usize,
    /// Added or removed lines, plus lines in untracked files.
    pub lines: usize,
    /// Newest modification time among the changed files still on disk.
    pub newest_edit_micros: Option<i64>,
}

const MAX_UNTRACKED_BYTES: u64 = 1_048_576;

fn modified_micros(metadata: &std::fs::Metadata) -> Option<i64> {
    let elapsed = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(elapsed.as_micros().min(i64::MAX as u128) as i64)
}

fn count_lines(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    bytes.iter().filter(|byte| **byte == b'\n').count() + usize::from(!bytes.ends_with(b"\n"))
}

/// Count changed files, changed lines and the newest edit in one worktree.
/// Every Git read is bounded and read-only. A failed read reports no stats
/// rather than turning an uncertain observation into a firm count.
pub fn edit_stats(
    context: &WorktreeContext<'_>,
    path: &Path,
    cancel: &CancelToken,
) -> Result<EditStats, WorktreeError> {
    let tree = super::custody::verify_probed_worktree(context, path, cancel)?;
    let status = super::inventory::run_git(
        context,
        tree.repo(),
        GitRead::StatusIncludingIgnored,
        cancel,
    )?;
    if super::inventory::exit_code(&status)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    let records =
        parse_porcelain_v2_z(&status.stdout.bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    let mut paths = BTreeSet::new();
    let mut untracked = BTreeSet::new();
    for record in records {
        if record.kind == StatusKind::Ignored {
            continue;
        }
        if record.kind == StatusKind::Untracked {
            untracked.insert(record.path.clone());
        }
        paths.insert(record.path);
    }
    let files = paths.len();

    let mut stats = EditStats {
        files,
        ..EditStats::default()
    };
    if files == 0 {
        return Ok(stats);
    }

    let patch = super::inventory::run_git(context, tree.repo(), GitRead::DirtyPatch, cancel)?;
    if super::inventory::exit_code(&patch)? == 0 {
        stats.lines = patch
            .stdout
            .bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| {
                matches!(line.first(), Some(b'+') | Some(b'-'))
                    && !line.starts_with(b"+++")
                    && !line.starts_with(b"---")
            })
            .count();
    }

    for raw in &paths {
        let relative = Path::new(std::ffi::OsStr::from_bytes(raw));
        if !relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        {
            continue;
        }
        let absolute = path.join(relative);
        let Ok(metadata) = std::fs::symlink_metadata(&absolute) else {
            continue;
        };
        if let Some(modified) = modified_micros(&metadata) {
            stats.newest_edit_micros = Some(
                stats
                    .newest_edit_micros
                    .map_or(modified, |newest| newest.max(modified)),
            );
        }
        // A diff against HEAD does not include untracked files. Count their
        // lines from the file itself, within a fixed read bound.
        if untracked.contains(raw)
            && metadata.is_file()
            && metadata.len() <= MAX_UNTRACKED_BYTES
            && let Ok(bytes) = std::fs::read(&absolute)
        {
            stats.lines += count_lines(&bytes);
        }
    }
    Ok(stats)
}
